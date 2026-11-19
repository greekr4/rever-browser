// A small Chrome DevTools Protocol endpoint in front of the Servo WebView, so
// rever's existing external-browser path (src/main/external-cdp.ts) can attach
// to this shell the way it attaches to Chrome. It implements only what that
// path calls; every other method answers with a "not implemented" error.
//
// Threads: one per connection. Network events arrive from Servo's network
// threads (see vendor/servo-net/tap.rs) and are broadcast directly; commands
// that touch the WebView are queued for the main thread as `MainCommand`.

use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use servo::JSValue;
use tungstenite::Message;

const TARGET_ID: &str = "servo-page-1";
/// Methods answered with `{}`: the shim has nothing to switch on or off for them.
const ACK_ONLY: &[&str] = &["Page.screencastFrameAck", "Runtime.runIfWaitingForDebugger"];
/// Everything the shim answers, as `Domain.name`. `/json/protocol` is built from this.
const COMMANDS: &[&str] = &[
    "Network.enable",
    "Network.disable",
    "Network.getResponseBody",
    "Page.enable",
    "Page.disable",
    "Page.navigate",
    "Page.addScriptToEvaluateOnNewDocument",
    "Page.startScreencast",
    "Page.stopScreencast",
    "Page.screencastFrameAck",
    "Runtime.enable",
    "Runtime.disable",
    "Runtime.evaluate",
    "Runtime.runIfWaitingForDebugger",
    "Debugger.enable",
    "Debugger.disable",
    "Input.dispatchMouseEvent",
    "Input.dispatchKeyEvent",
    "Browser.getVersion",
];
const EVENTS: &[&str] = &[
    "Network.requestWillBeSent",
    "Network.responseReceived",
    "Network.responseReceivedExtraInfo",
    "Network.loadingFinished",
    "Network.loadingFailed",
    "Network.webSocketCreated",
    "Network.webSocketWillSendHandshakeRequest",
    "Network.webSocketHandshakeResponseReceived",
    "Network.webSocketFrameSent",
    "Network.webSocketFrameReceived",
    "Page.screencastFrame",
    "Runtime.consoleAPICalled",
    "Runtime.exceptionThrown",
];
// shortcut: response bodies are kept for the last 400 requests only; move them
// to disk or size-cap them before pointing this at long sessions.
const MAX_BODIES: usize = 400;

/// Where to send the answer to one CDP command.
pub struct Reply {
    tx: Sender<String>,
    id: Value,
    session_id: Option<Value>,
}

impl Reply {
    pub fn ok(self, result: Value) {
        self.send(json!({ "id": self.id, "result": result }));
    }

    pub fn error(self, message: impl Into<String>) {
        self.send(json!({ "id": self.id, "error": { "code": -32000, "message": message.into() } }));
    }

    fn send(&self, mut message: Value) {
        if let Some(session_id) = &self.session_id {
            message["sessionId"] = session_id.clone();
        }
        let _ = self.tx.send(message.to_string());
    }
}

/// Work that must run on the main thread, where the WebView lives.
pub enum MainCommand {
    Navigate { url: String, reply: Reply },
    Evaluate { expression: String, reply: Reply },
    AddInitScript { source: String, reply: Reply },
    Screencast { on: bool, reply: Reply },
    Mouse { params: Value, reply: Reply },
    Key { params: Value, reply: Reply },
}

#[derive(Default)]
struct Bodies {
    order: VecDeque<String>,
    by_id: HashMap<String, (Vec<u8>, String)>,
}

pub struct Hub {
    port: u16,
    started: Instant,
    clients: Mutex<Vec<Sender<String>>>,
    bodies: Mutex<Bodies>,
    page: Mutex<(String, String)>,
    commands: Mutex<Sender<MainCommand>>,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Hub {
    /// Send one CDP event to every connected client.
    pub fn broadcast(&self, method: &str, params: Value) {
        let message = json!({ "method": method, "params": params }).to_string();
        self.clients
            .lock()
            .unwrap()
            .retain(|client| client.send(message.clone()).is_ok());
    }

    /// Seconds since the shim started: CDP's monotonic `timestamp`.
    pub fn timestamp(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn set_page(&self, url: Option<String>, title: Option<String>) {
        let mut page = self.page.lock().unwrap();
        if let Some(url) = url {
            page.0 = url;
        }
        if let Some(title) = title {
            page.1 = title;
        }
    }

    pub fn on_request(
        &self,
        id: &str,
        url: &str,
        method: &str,
        headers: Value,
        body: &[u8],
        destination: &str,
    ) {
        self.broadcast(
            "Network.requestWillBeSent",
            request_event(
                id,
                url,
                method,
                headers,
                body,
                destination,
                self.timestamp(),
            ),
        );
    }

    pub fn on_response(&self, id: &str, url: &str, status: u16, headers: Value, body: Vec<u8>) {
        let mime = mime_of(&headers);
        let timestamp = self.timestamp();
        self.broadcast(
            "Network.responseReceived",
            json!({
                "requestId": id,
                "timestamp": timestamp,
                "type": "Other",
                "response": { "url": url, "status": status, "statusText": "", "headers": headers, "mimeType": mime },
            }),
        );
        let length = body.len();
        {
            let mut bodies = self.bodies.lock().unwrap();
            if bodies.by_id.insert(id.to_owned(), (body, mime)).is_none() {
                bodies.order.push_back(id.to_owned());
            }
            while bodies.order.len() > MAX_BODIES {
                if let Some(oldest) = bodies.order.pop_front() {
                    bodies.by_id.remove(&oldest);
                }
            }
        }
        // Sent after the body is stored: clients ask for it as soon as this arrives.
        self.broadcast(
            "Network.loadingFinished",
            json!({ "requestId": id, "timestamp": timestamp, "encodedDataLength": length }),
        );
    }
}

/// Start listening on loopback and return the hub plus the main thread's command queue.
pub fn start(
    port: u16,
    wake: impl Fn() + Send + Sync + 'static,
) -> std::io::Result<(Arc<Hub>, Receiver<MainCommand>)> {
    let (commands, command_queue) = mpsc::channel();
    let hub = Arc::new(Hub {
        port,
        started: Instant::now(),
        clients: Mutex::default(),
        bodies: Mutex::default(),
        page: Mutex::new(("about:blank".into(), String::new())),
        commands: Mutex::new(commands),
        wake: Box::new(wake),
    });
    let v4 = TcpListener::bind(("127.0.0.1", port))?;
    // Node resolves `localhost` to ::1 first on some setups; Chrome listens on both too.
    let listeners = [Some(v4), TcpListener::bind(("::1", port)).ok()];
    for listener in listeners.into_iter().flatten() {
        let hub = hub.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let hub = hub.clone();
                std::thread::spawn(move || serve(stream, hub));
            }
        });
    }
    Ok((hub, command_queue))
}

fn serve(mut stream: TcpStream, hub: Arc<Hub>) {
    let mut head = [0u8; 2048];
    let Ok(n) = stream.peek(&mut head) else {
        return;
    };
    let head = String::from_utf8_lossy(&head[..n]);
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
    if !head.to_ascii_lowercase().contains("upgrade: websocket") {
        let _ = stream.read(&mut [0u8; 4096]);
        let (page_url, title) = hub.page.lock().unwrap().clone();
        let (status, body) = match http_json(&path, hub.port, &page_url, &title) {
            Some(body) => ("200 OK", body.to_string()),
            None => ("404 Not Found", "{}".to_owned()),
        };
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        return;
    }

    let Ok(mut socket) = tungstenite::accept(stream) else {
        return;
    };
    // A short read timeout lets one thread both read commands and flush events.
    let _ = socket
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(15)));
    let (tx, outbox) = mpsc::channel::<String>();
    hub.clients.lock().unwrap().push(tx.clone());
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => handle(text.as_str(), &hub, &tx),
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
        while let Ok(message) = outbox.try_recv() {
            if socket.send(Message::text(message)).is_err() {
                return;
            }
        }
    }
}

fn handle(text: &str, hub: &Hub, tx: &Sender<String>) {
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let method = message["method"].as_str().unwrap_or_default().to_owned();
    let params = message["params"].clone();
    let reply = Reply {
        tx: tx.clone(),
        id: message["id"].clone(),
        session_id: message.get("sessionId").cloned(),
    };
    let text_param = |name: &str| params[name].as_str().unwrap_or_default().to_owned();

    let command = match method.as_str() {
        "Network.getResponseBody" => {
            let bodies = hub.bodies.lock().unwrap();
            return match bodies
                .by_id
                .get(params["requestId"].as_str().unwrap_or_default())
            {
                Some((body, mime)) => {
                    let (body, base64_encoded) = body_payload(body, mime);
                    reply.ok(json!({ "body": body, "base64Encoded": base64_encoded }))
                }
                None => reply.error("No resource with given identifier found"),
            };
        }
        "Browser.getVersion" => {
            return reply.ok(json!({ "protocolVersion": "1.3", "product": "Servo/0.8.0" }));
        }
        "Page.navigate" => MainCommand::Navigate {
            url: text_param("url"),
            reply,
        },
        "Runtime.evaluate" => MainCommand::Evaluate {
            expression: text_param("expression"),
            reply,
        },
        "Page.addScriptToEvaluateOnNewDocument" => MainCommand::AddInitScript {
            source: text_param("source"),
            reply,
        },
        "Page.startScreencast" => MainCommand::Screencast { on: true, reply },
        "Page.stopScreencast" => MainCommand::Screencast { on: false, reply },
        "Input.dispatchMouseEvent" => MainCommand::Mouse { params, reply },
        "Input.dispatchKeyEvent" => MainCommand::Key { params, reply },
        name if name.ends_with(".enable")
            || name.ends_with(".disable")
            || ACK_ONLY.contains(&name) =>
        {
            return reply.ok(json!({}));
        }
        name => return reply.error(format!("'{name}' is not implemented by the Servo shim")),
    };
    if hub.commands.lock().unwrap().send(command).is_ok() {
        (hub.wake)();
    }
}

/// The `/json/*` discovery endpoints CDP clients read before opening the socket.
fn http_json(path: &str, port: u16, page_url: &str, title: &str) -> Option<Value> {
    let socket = |kind: &str, id: &str| format!("ws://127.0.0.1:{port}/devtools/{kind}/{id}");
    match path.trim_end_matches('/') {
        "/json/version" => Some(json!({
            "Browser": "Servo/0.8.0",
            "Protocol-Version": "1.3",
            "webSocketDebuggerUrl": socket("browser", "servo"),
        })),
        "/json" | "/json/list" => Some(json!([{
            "id": TARGET_ID,
            "type": "page",
            "title": title,
            "url": page_url,
            "webSocketDebuggerUrl": socket("page", TARGET_ID),
        }])),
        "/json/protocol" => Some(protocol_descriptor()),
        _ => None,
    }
}

/// Just the names: chrome-remote-interface builds its `client.Domain.method` API from this.
fn protocol_descriptor() -> Value {
    let mut domains: Vec<(String, Vec<Value>, Vec<Value>)> = Vec::new();
    for (names, is_command) in [(COMMANDS, true), (EVENTS, false)] {
        for name in names {
            let (domain, member) = name.split_once('.').expect("names are Domain.member");
            let index = domains
                .iter()
                .position(|d| d.0 == domain)
                .unwrap_or_else(|| {
                    domains.push((domain.to_owned(), Vec::new(), Vec::new()));
                    domains.len() - 1
                });
            let list = if is_command {
                &mut domains[index].1
            } else {
                &mut domains[index].2
            };
            list.push(json!({ "name": member }));
        }
    }
    json!({
        "version": { "major": "1", "minor": "3" },
        "domains": domains
            .into_iter()
            .map(|(domain, commands, events)| json!({ "domain": domain, "commands": commands, "events": events }))
            .collect::<Vec<_>>(),
    })
}

fn request_event(
    id: &str,
    url: &str,
    method: &str,
    headers: Value,
    body: &[u8],
    destination: &str,
    timestamp: f64,
) -> Value {
    let mut request = json!({ "url": url, "method": method, "headers": headers });
    if !body.is_empty() {
        request["postData"] = json!(String::from_utf8_lossy(body));
    }
    let wall_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    json!({
        "requestId": id,
        "loaderId": "",
        "documentURL": url,
        "request": request,
        "timestamp": timestamp,
        "wallTime": wall_time,
        "initiator": { "type": "other" },
        "type": resource_type(destination),
    })
}

/// Servo's fetch destination (Debug name) → CDP `Network.ResourceType`.
fn resource_type(destination: &str) -> &'static str {
    match destination {
        "Document" | "IFrame" | "Frame" => "Document",
        "Style" => "Stylesheet",
        "Script" | "Worker" | "SharedWorker" | "ServiceWorker" => "Script",
        "Image" => "Image",
        "Font" => "Font",
        "Audio" | "Video" | "Track" => "Media",
        "Manifest" => "Manifest",
        // XHR and fetch() have no destination.
        "None" => "XHR",
        _ => "Other",
    }
}

fn mime_of(headers: &Value) -> String {
    headers
        .as_object()
        .and_then(|map| {
            map.iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        })
        .and_then(|(_, value)| value.as_str())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default()
}

/// CDP returns text bodies as-is and everything else base64-encoded.
fn body_payload(body: &[u8], mime: &str) -> (String, bool) {
    let textual = mime.starts_with("text/")
        || ["json", "javascript", "xml", "x-www-form-urlencoded"]
            .iter()
            .any(|kind| mime.contains(kind));
    match std::str::from_utf8(body) {
        Ok(text) if textual => (text.to_owned(), false),
        _ => (BASE64.encode(body), true),
    }
}

/// Servo's script result → CDP `Runtime.RemoteObject`, always by value.
pub fn remote_object(value: &JSValue) -> Value {
    fn plain(value: &JSValue) -> Value {
        match value {
            JSValue::Undefined | JSValue::Null => Value::Null,
            JSValue::Boolean(b) => json!(b),
            JSValue::Number(n) => json!(n),
            JSValue::String(s)
            | JSValue::Element(s)
            | JSValue::ShadowRoot(s)
            | JSValue::Frame(s)
            | JSValue::Window(s) => json!(s),
            JSValue::Array(items) => Value::Array(items.iter().map(plain).collect()),
            JSValue::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| (key.clone(), plain(value)))
                    .collect(),
            ),
        }
    }
    match value {
        JSValue::Undefined => json!({ "type": "undefined" }),
        JSValue::Null => json!({ "type": "object", "subtype": "null", "value": null }),
        JSValue::Boolean(b) => json!({ "type": "boolean", "value": b }),
        JSValue::Number(n) => json!({ "type": "number", "value": n }),
        JSValue::String(s) => json!({ "type": "string", "value": s }),
        other => json!({ "type": "object", "value": plain(other) }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_endpoints_point_at_the_page_socket() {
        let list = http_json("/json/list", 9333, "https://a.test/", "A").unwrap();
        assert_eq!(list[0]["type"], "page");
        assert_eq!(
            list[0]["webSocketDebuggerUrl"],
            "ws://127.0.0.1:9333/devtools/page/servo-page-1"
        );
        assert!(http_json("/json/version", 9333, "", "").unwrap()["Browser"].is_string());
        assert!(http_json("/nope", 9333, "", "").is_none());
    }

    #[test]
    fn protocol_descriptor_lists_every_command_and_event_once() {
        let descriptor = protocol_descriptor();
        let names = |kind: &str| -> Vec<String> {
            descriptor["domains"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|domain| {
                    let prefix = domain["domain"].as_str().unwrap().to_owned();
                    domain[kind]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(move |member| format!("{prefix}.{}", member["name"].as_str().unwrap()))
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        let mut commands = names("commands");
        commands.sort();
        let mut expected: Vec<String> = COMMANDS.iter().map(|name| name.to_string()).collect();
        expected.sort();
        assert_eq!(commands, expected);
        assert_eq!(names("events").len(), EVENTS.len());
    }

    #[test]
    fn request_event_carries_post_data_and_maps_xhr() {
        let event = request_event(
            "r1",
            "https://a.test/api",
            "POST",
            json!({}),
            b"a=1",
            "None",
            1.5,
        );
        assert_eq!(event["request"]["postData"], "a=1");
        assert_eq!(event["type"], "XHR");
        let get = request_event(
            "r2",
            "https://a.test/",
            "GET",
            json!({}),
            b"",
            "Document",
            1.5,
        );
        assert!(get["request"].get("postData").is_none());
        assert_eq!(get["type"], "Document");
    }

    #[test]
    fn bodies_are_text_for_textual_mimes_and_base64_otherwise() {
        assert_eq!(
            body_payload(b"{\"n\":1}", "application/json"),
            ("{\"n\":1}".into(), false)
        );
        assert_eq!(
            body_payload(&[0x89, 0x50], "image/png"),
            ("iVA=".into(), true)
        );
        // Invalid UTF-8 under a text mime must not be mangled.
        assert_eq!(
            body_payload(&[0xff, 0xfe], "text/plain"),
            ("//4=".into(), true)
        );
        assert_eq!(
            mime_of(&json!({ "Content-Type": "Text/HTML; charset=utf-8" })),
            "text/html"
        );
    }

    #[test]
    fn script_results_become_by_value_remote_objects() {
        assert_eq!(
            remote_object(&JSValue::String("x".into())),
            json!({ "type": "string", "value": "x" })
        );
        assert_eq!(
            remote_object(&JSValue::Undefined),
            json!({ "type": "undefined" })
        );
        let array = JSValue::Array(vec![JSValue::Number(1.0), JSValue::Null]);
        assert_eq!(
            remote_object(&array),
            json!({ "type": "object", "value": [1.0, null] })
        );
    }
}
