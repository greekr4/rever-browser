//! The JavaScript debugger behind the breakpoint tools
//! (`rever_tools_debug::debugger::DebugEngine`), over Servo's own debugging
//! server: the Firefox remote debugging protocol on a local port that only this
//! client is admitted to (it presents the token Servo generated).
//!
//! Servo's side differs from Chrome's in ways this file absorbs:
//! - lines count from 1 (the tools count from 0);
//! - a breakpoint lands only on a script that is loaded, at one of the positions
//!   Servo lists for the line, so a breakpoint for a script that loads later is
//!   kept and set when the script appears;
//! - there are no conditional breakpoints: the condition is evaluated when the
//!   breakpoint is hit, and execution goes on when it is false;
//! - values come as previews, so an object is asked for again as JSON.
//!
//! shortcut: the debugger follows the tab that was selected when it first
//! connected; reconnect per tab when the tools learn to address tabs.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use regex::Regex;
use rever_tools_api::tool::BoxFuture;
use rever_tools_debug::debugger::{
    BreakpointRequest, DebugEngine, DebugState, FrameEval, SetBreakpoint,
};
use serde_json::{Value, json};

const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// The port of Servo's debugging server and its token, once it has started.
pub type Endpoint = Arc<Mutex<Option<(u16, String)>>>;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A breakpoint the agent asked for, and where it has been set so far.
struct Wanted {
    id: String,
    url: Regex,
    /// From 1, as Servo counts.
    line: u64,
    column: Option<u64>,
    condition: Option<String>,
    /// Script URL and column of each place it is set.
    set: Vec<(String, u64)>,
}

/// What the reader thread and the calls share.
#[derive(Default)]
struct Shared {
    /// Loaded scripts: source actor and URL.
    sources: Mutex<Vec<(String, String)>>,
    wanted: Mutex<Vec<Wanted>>,
    next_id: Mutex<u64>,
}

struct Connection {
    stream: TcpStream,
    replies: Receiver<Value>,
    thread: String,
    console: String,
    breakpoints: String,
}

pub struct ServoDebugger {
    endpoint: Endpoint,
    state: Weak<DebugState>,
    shared: Arc<Shared>,
    /// Held for a whole request and its reply: the protocol has no request ids.
    connection: Mutex<Option<Connection>>,
    /// To hand to the threads that follow up on what the page reports.
    this: Weak<ServoDebugger>,
}

fn write_packet(stream: &mut TcpStream, packet: &Value) -> Result<(), String> {
    let text = packet.to_string();
    stream
        .write_all(format!("{}:{text}", text.len()).as_bytes())
        .map_err(|e| format!("the debugging server went away: {e}"))
}

/// One `length:json` packet; `None` when the connection has ended.
fn read_packet(reader: &mut BufReader<TcpStream>) -> Option<Value> {
    let mut length = Vec::new();
    reader.read_until(b':', &mut length).ok()?;
    let length: usize = std::str::from_utf8(length.strip_suffix(b":")?)
        .ok()?
        .parse()
        .ok()?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// A whole number is an integer in the tools' JSON, as `JSON.stringify` prints it.
fn number(value: &Value) -> Value {
    match value.as_f64() {
        Some(n) if n.fract() == 0.0 && n.abs() < 9e15 => json!(n as i64),
        _ => value.clone(),
    }
}

impl Connection {
    /// Send `packet` and wait for the reply `accept` recognises; what arrives
    /// before it belongs to an earlier exchange and is dropped.
    fn request(&mut self, packet: Value, accept: impl Fn(&Value) -> bool) -> Result<Value, String> {
        write_packet(&mut self.stream, &packet)?;
        loop {
            let reply = self
                .replies
                .recv_timeout(REPLY_TIMEOUT)
                .map_err(|_| "the debugging server did not answer".to_owned())?;
            if accept(&reply) {
                return Ok(reply);
            }
        }
    }

    /// A request whose reply is the plain acknowledgement of the actor it went to.
    fn tell(&mut self, to: &str, mut packet: Value) -> Result<Value, String> {
        packet["to"] = json!(to);
        let from = to.to_owned();
        self.request(packet, move |reply| {
            reply["from"] == from.as_str() && reply.get("type").is_none()
        })
    }

    fn evaluate(&mut self, frame: &str, expression: &str) -> Result<Value, String> {
        let packet = json!({
            "to": self.console,
            "type": "evaluateJSAsync",
            "text": expression,
            "frameActor": frame,
        });
        self.request(packet, |reply| reply["type"] == "evaluationResult")
    }
}

impl ServoDebugger {
    pub fn new(endpoint: Endpoint, state: Weak<DebugState>) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            endpoint,
            state,
            shared: Arc::default(),
            connection: Mutex::new(None),
            this: this.clone(),
        })
    }

    /// Run `call` on the connection, making it first if there is none; a
    /// connection that failed is dropped so the next call starts a new one.
    fn with<T>(
        &self,
        call: impl FnOnce(&mut Connection, &Shared) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut connection = locked(&self.connection);
        if connection.is_none() {
            *connection = Some(self.connect()?);
        }
        let Some(open) = connection.as_mut() else {
            return Err("no debugging connection".to_owned());
        };
        let result = call(open, &self.shared);
        if result.as_ref().is_err_and(|e| e.contains("went away")) {
            *connection = None;
        }
        result
    }

    fn connect(&self) -> Result<Connection, String> {
        let (port, token) = locked(&self.endpoint)
            .clone()
            .ok_or("the page engine's debugging server has not started")?;
        let mut stream = TcpStream::connect(("127.0.0.1", port))
            .map_err(|e| format!("cannot reach the debugging server: {e}"))?;
        // The token admits this client without a prompt; it must be sent in
        // exactly this form.
        let auth = format!("{{\"auth_token\":\"{token}\"}}");
        stream
            .write_all(format!("{}:{auth}", auth.len()).as_bytes())
            .map_err(|e| e.to_string())?;

        let (forward, replies) = channel();
        let reading = stream.try_clone().map_err(|e| e.to_string())?;
        let (shared, this) = (self.shared.clone(), self.this.clone());
        std::thread::spawn(move || read_loop(reading, forward, shared, this));

        let mut connection = Connection {
            stream,
            replies,
            thread: String::new(),
            console: String::new(),
            breakpoints: String::new(),
        };
        connection.request(json!({ "to": "root", "type": "listTabs" }), |reply| {
            reply["tabs"].is_array()
        })
        .and_then(|listed| {
            let tabs = listed["tabs"].as_array().cloned().unwrap_or_default();
            let tab = tabs
                .iter()
                .find(|tab| tab["selected"] == true)
                .or(tabs.first())
                .and_then(|tab| tab["actor"].as_str())
                .ok_or("the debugging server lists no page")?
                .to_owned();
            let watcher = connection.request(
                json!({ "to": tab, "type": "getWatcher", "isServerTargetSwitchingEnabled": true }),
                |reply| reply["actor"].is_string(),
            )?;
            let watcher = watcher["actor"].as_str().unwrap_or_default().to_owned();
            let target = connection.request(
                json!({ "to": watcher, "type": "watchTargets", "targetType": "frame" }),
                |reply| reply["type"] == "target-available-form",
            )?;
            connection.thread = target["target"]["threadActor"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            connection.console = target["target"]["consoleActor"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let list = connection.request(
                json!({ "to": watcher, "type": "getBreakpointListActor" }),
                |reply| reply["breakpointList"].is_object(),
            )?;
            connection.breakpoints = list["breakpointList"]["actor"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            // The scripts already loaded arrive as an event the reader keeps.
            write_packet(
                &mut connection.stream,
                &json!({ "to": watcher, "type": "watchResources", "resourceTypes": ["source"] }),
            )?;
            // Attaching pauses the page once; it has to be let go again.
            let thread = connection.thread.clone();
            connection.tell(&thread, json!({ "type": "attach", "options": {} }))?;
            connection.tell(&thread, json!({ "type": "resume" }))?;
            Ok(())
        })?;
        Ok(connection)
    }

    /// Set every wanted breakpoint on every loaded script it matches and is not
    /// set on yet. Returns the places set by this call, as the tools print them.
    fn apply(connection: &mut Connection, shared: &Shared, only: Option<&str>) -> Vec<Value> {
        let sources = locked(&shared.sources).clone();
        let mut placed = Vec::new();
        let mut wanted = locked(&shared.wanted);
        for want in wanted
            .iter_mut()
            .filter(|w| only.is_none_or(|id| w.id == id))
        {
            for (actor, url) in &sources {
                if !want.url.is_match(url) || want.set.iter().any(|(set, _)| set == url) {
                    continue;
                }
                let positions = connection.request(
                    json!({ "to": actor, "type": "getBreakpointPositionsCompressed" }),
                    |reply| reply["positions"].is_object(),
                );
                let Ok(positions) = positions else { continue };
                let columns: Vec<u64> = positions["positions"][want.line.to_string()]
                    .as_array()
                    .map(|columns| columns.iter().filter_map(Value::as_u64).collect())
                    .unwrap_or_default();
                // The column asked for when the line can stop there, else its first stop.
                let column = want
                    .column
                    .filter(|column| columns.contains(column))
                    .or(columns.first().copied());
                let Some(column) = column else { continue };
                let location = json!({ "sourceUrl": url, "line": want.line, "column": column });
                let breakpoints = connection.breakpoints.clone();
                let set = connection.tell(
                    &breakpoints,
                    json!({ "type": "setBreakpoint", "location": location, "options": {} }),
                );
                if set.is_ok() {
                    want.set.push((url.clone(), column));
                    placed.push(json!({
                        "url": url,
                        "lineNumber": want.line - 1,
                        "columnNumber": column,
                    }));
                }
            }
        }
        placed
    }

    /// The page stopped: report it to the tools, unless it stopped on a
    /// breakpoint whose condition is false, in which case it goes on.
    fn on_paused(&self, why: &str) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let paused = self.with(|connection, shared| {
            let thread = connection.thread.clone();
            let listed = connection.request(
                json!({ "to": thread, "type": "frames", "start": 0, "count": 50 }),
                |reply| reply["frames"].is_array(),
            )?;
            let frames = listed["frames"].as_array().cloned().unwrap_or_default();
            let sources = locked(&shared.sources).clone();
            let url_of = |frame: &Value| {
                let actor = frame["where"]["actor"].as_str().unwrap_or_default();
                let found = sources.iter().find(|(source, _)| source == actor);
                found.map(|(_, url)| url.clone()).unwrap_or_default()
            };

            if let Some(top) = frames.first() {
                let (url, line) = (url_of(top), top["where"]["line"].as_u64().unwrap_or(0));
                let condition = locked(&shared.wanted)
                    .iter()
                    .find(|w| w.line == line && w.set.iter().any(|(set, _)| *set == url))
                    .and_then(|w| w.condition.clone());
                if let Some(condition) = condition {
                    let actor = top["actor"].as_str().unwrap_or_default();
                    let held = connection.evaluate(actor, &format!("!!({condition})"))?;
                    if held["result"] != true {
                        connection.tell(&thread, json!({ "type": "resume" }))?;
                        return Ok(None);
                    }
                }
            }

            let mut call_frames = Vec::new();
            for frame in &frames {
                let actor = frame["actor"].as_str().unwrap_or_default();
                // The scopes around the frame, innermost first.
                let mut scopes = Vec::new();
                let environment = connection.request(
                    json!({ "to": actor, "type": "getEnvironment" }),
                    |reply| reply["scopeKind"].is_string() || reply["actor"].is_string(),
                );
                let mut scope = environment.ok();
                while let Some(current) = scope {
                    scopes.push(json!({ "type": current["scopeKind"].as_str().unwrap_or("block") }));
                    scope = current.get("parent").filter(|p| p.is_object()).cloned();
                }
                call_frames.push(json!({
                    "callFrameId": actor,
                    "functionName": frame["displayName"].as_str().unwrap_or_default(),
                    "location": {
                        "scriptId": frame["where"]["actor"],
                        "url": url_of(frame),
                        "lineNumber": frame["where"]["line"].as_u64().unwrap_or(1).saturating_sub(1),
                        "columnNumber": frame["where"]["column"],
                    },
                    "scopeChain": scopes,
                }));
            }
            Ok(Some(json!({ "callFrames": call_frames, "reason": why })))
        });
        match paused {
            Ok(Some(paused)) => state.set_paused(serde_json::from_value(paused).ok()),
            Ok(None) => {}
            Err(error) => eprintln!("[debugger] {error}"),
        }
    }

    fn resume_with(&self, limit: Option<&'static str>) -> BoxFuture<'_, Result<(), String>> {
        let done = self.with(|connection, _| {
            let mut packet = json!({ "type": "resume" });
            if let Some(limit) = limit {
                packet["resumeLimit"] = json!({ "type": limit });
            }
            let thread = connection.thread.clone();
            connection.tell(&thread, packet).map(|_| ())
        });
        if done.is_ok()
            && let Some(state) = self.state.upgrade()
        {
            state.set_paused(None);
        }
        Box::pin(async move { done })
    }
}

/// Reads what the server sends: keeps the script list, starts the follow-up on
/// a pause, and hands everything else to the call that is waiting for a reply.
fn read_loop(
    stream: TcpStream,
    forward: Sender<Value>,
    shared: Arc<Shared>,
    debugger: Weak<ServoDebugger>,
) {
    let mut reader = BufReader::new(stream);
    while let Some(packet) = read_packet(&mut reader) {
        match packet["type"].as_str() {
            Some("resources-available-array") => {
                let mut added = false;
                for group in packet["array"].as_array().into_iter().flatten() {
                    if group[0] != "source" {
                        continue;
                    }
                    for source in group[1].as_array().into_iter().flatten() {
                        let (Some(actor), Some(url)) =
                            (source["actor"].as_str(), source["url"].as_str())
                        else {
                            continue;
                        };
                        locked(&shared.sources).push((actor.to_owned(), url.to_owned()));
                        added = true;
                    }
                }
                // A breakpoint may have been waiting for one of these scripts.
                if added && let Some(debugger) = debugger.upgrade() {
                    std::thread::spawn(move || {
                        let _ = debugger.with(|connection, shared| {
                            ServoDebugger::apply(connection, shared, None);
                            Ok(())
                        });
                    });
                }
            }
            // Attaching pauses the page by itself; the connection resumes it.
            Some("paused") if packet["why"]["type"] != "attached" => {
                let why = packet["why"]["type"].as_str().unwrap_or("other").to_owned();
                if let Some(debugger) = debugger.upgrade() {
                    std::thread::spawn(move || debugger.on_paused(&why));
                }
            }
            Some("paused" | "resumed" | "frameUpdate") => {}
            _ => {
                if forward.send(packet).is_err() {
                    break;
                }
            }
        }
    }
}

impl DebugEngine for ServoDebugger {
    fn has_target(&self) -> bool {
        locked(&self.endpoint).is_some()
    }

    fn set_breakpoint<'a>(
        &'a self,
        request: &'a BreakpointRequest,
    ) -> BoxFuture<'a, Result<SetBreakpoint, String>> {
        let set = Regex::new(&request.url_regex)
            .map_err(|e| format!("invalid URL pattern: {e}"))
            .and_then(|url| {
                self.with(|connection, shared| {
                    let id = {
                        let mut next = locked(&shared.next_id);
                        *next += 1;
                        format!("servo:{}", *next)
                    };
                    locked(&shared.wanted).push(Wanted {
                        id: id.clone(),
                        url,
                        line: request.line_number + 1,
                        column: request.column_number,
                        condition: request.condition.clone(),
                        set: Vec::new(),
                    });
                    // No place yet is not an error: the script may load later.
                    let locations = Self::apply(connection, shared, Some(&id));
                    Ok(SetBreakpoint {
                        breakpoint_id: id,
                        locations,
                    })
                })
            });
        Box::pin(async move { set })
    }

    fn remove_breakpoint<'a>(
        &'a self,
        breakpoint_id: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        let removed = self.with(|connection, shared| {
            let mut wanted = locked(&shared.wanted);
            let Some(at) = wanted.iter().position(|w| w.id == breakpoint_id) else {
                return Ok(());
            };
            let want = wanted.remove(at);
            drop(wanted);
            let breakpoints = connection.breakpoints.clone();
            for (url, column) in want.set {
                let location = json!({ "sourceUrl": url, "line": want.line, "column": column });
                connection.tell(
                    &breakpoints,
                    json!({ "type": "removeBreakpoint", "location": location }),
                )?;
            }
            Ok(())
        });
        Box::pin(async move { removed })
    }

    fn resume(&self) -> BoxFuture<'_, Result<(), String>> {
        self.resume_with(None)
    }

    fn step_over(&self) -> BoxFuture<'_, Result<(), String>> {
        self.resume_with(Some("next"))
    }

    fn step_into(&self) -> BoxFuture<'_, Result<(), String>> {
        self.resume_with(Some("step"))
    }

    fn step_out(&self) -> BoxFuture<'_, Result<(), String>> {
        self.resume_with(Some("finish"))
    }

    fn evaluate_on_call_frame<'a>(
        &'a self,
        call_frame_id: &'a str,
        expression: &'a str,
    ) -> BoxFuture<'a, Result<FrameEval, String>> {
        let evaluated = self.with(|connection, _| {
            let reply = connection.evaluate(call_frame_id, expression)?;
            if reply["hasException"] == true {
                let message = reply["exceptionMessage"].as_str().unwrap_or("exception");
                return Ok(FrameEval {
                    exception: Some(message.to_owned()),
                    ..FrameEval::default()
                });
            }
            let result = &reply["result"];
            let Some(grip) = result.as_object() else {
                // A string, a number or a boolean arrives as itself.
                return Ok(FrameEval {
                    value: Some(number(result)),
                    description: Some(match result.as_str() {
                        Some(text) => text.to_owned(),
                        None => number(result).to_string(),
                    }),
                    exception: None,
                });
            };
            match grip.get("type").and_then(Value::as_str) {
                Some("undefined") => Ok(FrameEval {
                    description: Some("undefined".to_owned()),
                    ..FrameEval::default()
                }),
                Some("null") => Ok(FrameEval {
                    value: Some(Value::Null),
                    description: Some("null".to_owned()),
                    exception: None,
                }),
                // An object comes as a preview: ask for it again as JSON, which
                // is what "by value" means to the tools.
                _ => {
                    let again = connection
                        .evaluate(call_frame_id, &format!("JSON.stringify(({expression}))"))?;
                    let by_value = again["result"]
                        .as_str()
                        .and_then(|text| serde_json::from_str(text).ok());
                    Ok(FrameEval {
                        value: by_value,
                        description: grip.get("class").and_then(Value::as_str).map(str::to_owned),
                        exception: None,
                    })
                }
            }
        });
        Box::pin(async move { evaluated })
    }
}
