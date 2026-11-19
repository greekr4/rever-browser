//! Port of `src/main/mcp/tools/websocket.ts` — list_websockets, get_ws_frames,
//! ws_capture_enable, ws_capture_list and ws_send.
//!
//! The TypeScript tools call into `src/main/ws-capture.ts`. The part of it that
//! works through a constructor wrapper (install it, send by capture id) is
//! here, over `Browser::add_init_script` and `Browser::evaluate`. The part that
//! reads live sockets out of the page heap (Runtime.queryObjects) has no
//! `Browser` counterpart, so ws_capture_list and ws_send by url report that.

use std::sync::Mutex;

use rever_core::traffic::ListFilter;
use rever_tools_api::tool::{ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, ok};
use serde_json::{Value, json};

use crate::args::{Args, MAX_SAFE_INTEGER, limit_prop, no_input, prop, schema};
use crate::js::{utf16_len, utf16_prefix};
use crate::{lock, ok_pretty, store};

const MAX_PAYLOAD_BYTES: usize = 1024;

tool!(
    ListWebsockets,
    ToolDef::new(
        "list_websockets",
        "List captured WebSocket connections.",
        schema(
            json!({
                "host": prop("string", "Substring host filter"),
                "limit": limit_prop("Max items (default 50)", 200)
            }),
            &[],
        ),
    ),
    |ctx, args| list_websockets(ctx, &args)
);

fn list_websockets(ctx: &ToolContext, args: &Args) -> Result<ToolResult, String> {
    let filter = ListFilter {
        host: args.str("host")?.map(str::to_owned),
        limit: Some(args.limit(200)?.unwrap_or(50)),
        ..ListFilter::default()
    };
    let store = store(ctx);
    // The limit counts requests of every type; the sockets among them are listed.
    let sockets: Vec<Value> = store
        .list(&filter)
        .into_iter()
        .filter(|r| r.resource_type == "WebSocket")
        .map(|r| {
            let mut row = json!({
                "requestId": r.request_id,
                "url": r.url,
                "host": r.host,
                "startedAt": r.started_at
            });
            if let (Some(completed_at), Some(row)) = (r.completed_at, row.as_object_mut()) {
                row.insert("completedAt".to_owned(), completed_at.into());
            }
            row
        })
        .collect();
    ok_pretty(&sockets)
}

tool!(
    GetWsFrames,
    ToolDef::new(
        "get_ws_frames",
        "Return WebSocket frames for a given connection. Large payloads are truncated at 1KB.",
        schema(
            json!({
                "requestId": { "type": "string", "description": "requestId of the WebSocket connection" },
                "since": prop("number", "Only include frames after this epoch ms"),
                "limit": limit_prop("Max frames (default 100)", 500)
            }),
            &["requestId"],
        ),
    ),
    |ctx, args| get_ws_frames(ctx, &args)
);

fn get_ws_frames(ctx: &ToolContext, args: &Args) -> Result<ToolResult, String> {
    let request_id = args.required_str("requestId")?;
    let since = args.since()?;
    let limit = args.limit(500)?.unwrap_or(100);
    let store = store(ctx);
    // Check the request exists first, so a wrong id gets a clear error
    // instead of an empty list.
    if store.get(request_id).is_none() {
        return Err(format!(
            "unknown requestId: {request_id} — use list_websockets to find valid IDs"
        ));
    }
    let frames = store.ws_frames(request_id, since);
    let newest = frames
        .get(frames.len().saturating_sub(limit)..)
        .unwrap_or_default();
    let formatted: Vec<Value> = newest
        .iter()
        .map(|f| {
            let total = utf16_len(&f.payload_data);
            let truncated = total > MAX_PAYLOAD_BYTES;
            let mut frame = serde_json::Map::new();
            frame.insert("direction".to_owned(), json!(f.direction));
            frame.insert("opcode".to_owned(), f.opcode.into());
            frame.insert("timestamp".to_owned(), f.timestamp.into());
            if let Some(mask) = f.mask {
                frame.insert("mask".to_owned(), mask.into());
            }
            frame.insert(
                "payload".to_owned(),
                utf16_prefix(&f.payload_data, MAX_PAYLOAD_BYTES).into(),
            );
            if truncated {
                frame.insert("truncated".to_owned(), true.into());
                frame.insert("totalBytes".to_owned(), total.into());
            }
            Value::Object(frame)
        })
        .collect();
    ok_pretty(&formatted)
}

// ── From ws-capture.ts: the constructor wrapper ──────────────────────────────
// A wrapper around the WebSocket constructor keeps every socket the page opens,
// so a frame can be sent on the page's own socket (auth and session are bound
// to it).

/// The constructor wrapper `ws_capture_enable` installs. Public so the app
/// can install it as an init script at startup, which makes the sockets list
/// and url-addressable sends work on every page without an explicit enable.
pub const WS_CAPTURE_SCRIPT: &str = r#"(() => {
  if (window.__revWSCapture) return;
  const list = [];
  window.__revWSCapture = list;
  const OrigWS = window.WebSocket;
  function Wrapped(url, protocols) {
    const ws = protocols == null ? new OrigWS(url) : new OrigWS(url, protocols);
    const entry = { id: list.length, url: String(url), ws, openedAt: Date.now() };
    list.push(entry);
    return ws;
  }
  Wrapped.prototype = OrigWS.prototype;
  Wrapped.CONNECTING = OrigWS.CONNECTING;
  Wrapped.OPEN = OrigWS.OPEN;
  Wrapped.CLOSING = OrigWS.CLOSING;
  Wrapped.CLOSED = OrigWS.CLOSED;
  window.WebSocket = Wrapped;
})();"#;

/// The init script registered by ws_capture_enable, so enabling again replaces
/// it instead of stacking a second one. The TypeScript keeps one id per
/// webContents; `Browser` drives a single tab, so there is one slot.
#[derive(Default)]
pub struct WsCaptureState {
    script_id: Mutex<Option<String>>,
}

/// The expression ws_send runs in the page (text and binary paths).
fn build_send_expression(capture_id: i64, payload: &str, as_binary: bool) -> String {
    // JSON.stringify(payload): a JavaScript string literal.
    let payload = Value::from(payload).to_string();
    if as_binary {
        format!(
            "(() => {{
        const e = (window.__revWSCapture || [])[{capture_id}];
        if (!e) return 'no-such-capture';
        if (e.ws.readyState !== 1) return 'not-open:' + e.ws.readyState;
        const raw = atob({payload});
        const buf = new Uint8Array(raw.length);
        for (let i = 0; i < raw.length; i++) buf[i] = raw.charCodeAt(i);
        e.ws.send(buf);
        return 'ok';
      }})()"
        )
    } else {
        format!(
            "(() => {{
        const e = (window.__revWSCapture || [])[{capture_id}];
        if (!e) return 'no-such-capture';
        if (e.ws.readyState !== 1) return 'not-open:' + e.ws.readyState;
        e.ws.send({payload});
        return 'ok';
      }})()"
        )
    }
}

tool!(
    WsCaptureEnable,
    ToolDef::new(
        "ws_capture_enable",
        "Legacy: install a WebSocket constructor wrapper (needs a page reload). Only needed for ws_send by captureId — ws_send with url reaches already-open sockets without it.",
        no_input(),
    ),
    |ctx, _args| ws_capture_enable(ctx).await
);

async fn ws_capture_enable(ctx: &ToolContext) -> Result<ToolResult, String> {
    let state = ctx.state::<WsCaptureState>();
    // Remove the script registered before, if any. Taken out of the lock
    // first: the guard cannot be held across an await.
    let previous = lock(&state.script_id).take();
    if let Some(previous) = previous {
        let _ = ctx.browser.remove_init_script(&previous).await;
    }
    let id = ctx.browser.add_init_script(WS_CAPTURE_SCRIPT).await?;
    *lock(&state.script_id) = Some(id);
    // Install into the current document too (a no-op when it is already there).
    ctx.browser.evaluate(WS_CAPTURE_SCRIPT).await?;
    Ok(ok(
        "ws-capture installed; reload page to wrap pre-existing sockets",
    ))
}

tool!(
    WsCaptureList,
    ToolDef::new(
        "ws_capture_list",
        "List the page's live WebSockets found in memory. No setup or reload needed: when the ws_capture_enable wrapper is installed (installing it into the current document takes one call) every socket the page opens is held in window.__revWSCapture and read from there. Use a url from here with ws_send.",
        no_input(),
    ),
    |ctx, _args| ws_capture_list(ctx).await
);

/// The wrapper's entries, as the page sees them. `readyState` is kept so a
/// closed-but-recorded socket is still visible (it explains past frames).
const LIST_EXPRESSION: &str = "JSON.stringify((window.__revWSCapture||[]).map(e=>\
({id:e.id,url:e.url,readyState:e.ws.readyState,openedAt:e.openedAt,protocols:e.protocols||null})))";

/// One evaluate that installs the wrapper when this document does not have it
/// yet, then lists what it holds. What the WebSocket panel polls.
pub fn sockets_expression() -> String {
    format!("{WS_CAPTURE_SCRIPT};{LIST_EXPRESSION}")
}

async fn ws_capture_list(ctx: &ToolContext) -> Result<ToolResult, String> {
    match ctx.browser.evaluate(LIST_EXPRESSION).await? {
        Value::String(json) => {
            let rows: Vec<Value> = serde_json::from_str(&json)
                .map_err(|error| format!("ws_capture_list: unreadable wrapper state: {error}"))?;
            ok_pretty(&rows)
        }
        other => Err(format!(
            "ws-capture is not installed in this document — call ws_capture_enable first (got {other})"
        )),
    }
}

tool!(
    WsSend,
    ToolDef::new(
        "ws_send",
        "Send a frame on the page's own live WebSocket (auth and session preserved). Pass url (from ws_capture_list or list_websockets) — after one ws_capture_enable call it reaches sockets the page already opened, including in cross-site iframes. Payload is sent as a UTF-8 string; for binary, base64-encode the bytes and set asBinary=true. If the socket has closed, reconnect=true reopens it from the page (same URL, cookies and origin) and sends on the new connection, which shows up in list_websockets under a new requestId. captureId is the legacy path after ws_capture_enable.",
        schema(
            json!({
                "url": prop("string", "Socket URL, e.g. wss://host/socket?room=1"),
                "captureId": {
                    "description": "Legacy: id from the ws_capture_enable wrapper",
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_SAFE_INTEGER
                },
                "payload": {
                    "type": "string",
                    "description": "Message payload (text or base64 when asBinary=true)"
                },
                "asBinary": prop("boolean", "Treat payload as base64-encoded bytes"),
                "reconnect": prop(
                    "boolean",
                    "With url: if the socket has closed, reopen it from the page and send on the new connection"
                )
            }),
            &["payload"],
        ),
    ),
    |ctx, args| ws_send(ctx, &args).await
);

/// Send on the socket the wrapper holds for `url` — the page's own socket, so
/// auth and session ride with it. `reconnect` reopens (and records) a new one
/// when the old socket closed or the wrapper never saw it.
/// The expression ws_send runs by url — find the wrapper's socket, send on
/// it, reopen when `reconnect` and it closed. Public for the panel's host.
pub fn send_by_url_expression(
    url: &str,
    payload: &str,
    as_binary: bool,
    reconnect: bool,
) -> String {
    // asBinary: base64 → bytes. Text goes as a JSON string literal.
    let payload = if as_binary {
        format!("(()=>{{const raw=atob({});const b=new Uint8Array(raw.length);\
for(let i=0;i<raw.length;i++)b[i]=raw.charCodeAt(i);return b}})()",
            Value::from(payload).to_string())
    } else {
        Value::from(payload).to_string()
    };
    let expression = format!(
        "(() => {{\
const list = window.__revWSCapture || [];\
const url = {url};\
let e = list.find(x => x.url === url);\
if (e && e.ws.readyState !== 1) {{ if (!{reconnect}) return 'not-open:' + e.ws.readyState; e = null; }}\
if (!e) {{ if (!{reconnect}) return 'no-such-capture'; \
e = {{ id: list.length, url, ws: new WebSocket(url), openedAt: Date.now() }}; list.push(e); \
e.ws.onopen = () => e.ws.send({payload}); return 'reopened'; }}\
e.ws.send({payload}); return 'ok'; }})()",
        url = Value::from(url).to_string(),
        reconnect = reconnect,
        payload = payload,
    );
    expression
}

/// What `send_by_url_expression` answers, mapped to the tool's result.
async fn ws_send_by_url(
    ctx: &ToolContext,
    url: &str,
    payload: &str,
    as_binary: bool,
    reconnect: bool,
) -> Result<ToolResult, String> {
    let expression = send_by_url_expression(url, payload, as_binary, reconnect);
    match ctx.browser.evaluate(&expression).await? {
        Value::String(answer) if answer == "ok" => Ok(ok("sent on the page socket".to_owned())),
        Value::String(answer) if answer == "reopened" => {
            Ok(ok("socket closed — reopened it from the page and sent on the new connection".to_owned()))
        }
        Value::String(answer) if answer == "no-such-capture" => Err(
            "no wrapped socket at this url — call ws_capture_enable first (sockets opened after \
             that are addressable by url), or pass reconnect: true to open a new one"
                .to_owned(),
        ),
        Value::String(answer) => Err(format!("ws_send failed: {answer}")),
        other => Err(format!("ws_send failed: {other}")),
    }
}

async fn ws_send(ctx: &ToolContext, args: &Args<'_>) -> Result<ToolResult, String> {
    let url = args.str("url")?;
    let capture_id = args.int("captureId", 0, MAX_SAFE_INTEGER)?;
    let payload = args.required_str("payload")?;
    let as_binary = args.bool("asBinary")?.unwrap_or(false);
    args.bool("reconnect")?;

    if let Some(url) = url {
        let reconnect = args.bool("reconnect")?.unwrap_or(false);
        return ws_send_by_url(ctx, url, payload, as_binary, reconnect).await;
    }
    let Some(capture_id) = capture_id else {
        return Err("pass url (preferred) or captureId".to_owned());
    };
    let expression = build_send_expression(capture_id, payload, as_binary);
    match ctx.browser.evaluate(&expression).await? {
        Value::String(answer) if answer == "ok" => Ok(ok(format!("sent to ws#{capture_id}"))),
        Value::String(answer) => Err(format!("ws_send failed: {answer}")),
        other => Err(format!("ws_send failed: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rever_core::traffic::{WsDirection, WsFrame};
    use rever_tools_api::tool::{ToolContext, unsupported};

    use super::*;
    use crate::test_support::{FakeBrowser, call, call_json, ctx_with, put, request};

    fn frame(direction: WsDirection, payload: &str, timestamp: u64) -> WsFrame {
        WsFrame {
            direction,
            opcode: 1,
            payload_data: payload.to_owned(),
            timestamp,
            mask: None,
        }
    }

    fn socket_ctx() -> ToolContext {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("ws1", "wss://chat.a.test/socket", "GET", "WebSocket"),
            100,
        );
        ctx
    }

    #[tokio::test]
    async fn 웹소켓_연결만_골라_보여준다() {
        let ctx = socket_ctx();
        put(&ctx, request("x", "https://a.test/api", "GET", "XHR"), 200);
        let mut closed = request("ws2", "wss://b.test/live", "GET", "WebSocket");
        closed.completed_at = Some(450);
        put(&ctx, closed, 300);

        assert_eq!(
            call(&ListWebsockets, &ctx, json!({})).await.unwrap(),
            r#"[
  {
    "requestId": "ws2",
    "url": "wss://b.test/live",
    "host": "b.test",
    "startedAt": 300,
    "completedAt": 450
  },
  {
    "requestId": "ws1",
    "url": "wss://chat.a.test/socket",
    "host": "chat.a.test",
    "startedAt": 100
  }
]"#
        );
        let by_host = call_json(&ListWebsockets, &ctx, json!({ "host": "a.test" })).await;
        assert_eq!(by_host.as_array().map(Vec::len), Some(1));
        // The limit is applied before the type filter, as in the TypeScript:
        // the two newest requests hold one socket.
        let limited = call_json(&ListWebsockets, &ctx, json!({ "limit": 2 })).await;
        assert_eq!(limited.as_array().map(Vec::len), Some(1));
        assert!(
            call(&ListWebsockets, &ctx, json!({ "limit": 201 }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn 프레임을_오래된_순으로_돌려주고_mask는_있을_때만_적는다() {
        let ctx = socket_ctx();
        {
            let mut store = lock(&ctx.traffic);
            store.append_ws_frame("ws1", frame(WsDirection::Sent, "hello", 110));
            let mut masked = frame(WsDirection::Received, "world", 120);
            masked.mask = Some(false);
            masked.opcode = 2;
            store.append_ws_frame("ws1", masked);
        }
        assert_eq!(
            call(&GetWsFrames, &ctx, json!({ "requestId": "ws1" }))
                .await
                .unwrap(),
            r#"[
  {
    "direction": "sent",
    "opcode": 1,
    "timestamp": 110,
    "payload": "hello"
  },
  {
    "direction": "received",
    "opcode": 2,
    "timestamp": 120,
    "mask": false,
    "payload": "world"
  }
]"#
        );
    }

    #[tokio::test]
    async fn 긴_페이로드는_1kb에서_자르고_전체_길이를_적는다() {
        let ctx = socket_ctx();
        {
            let mut store = lock(&ctx.traffic);
            store.append_ws_frame("ws1", frame(WsDirection::Sent, &"a".repeat(1024), 110));
            // 1025 UTF-16 units (3075 bytes): the limit counts units.
            store.append_ws_frame("ws1", frame(WsDirection::Sent, &"한".repeat(1025), 120));
        }
        let frames = call_json(&GetWsFrames, &ctx, json!({ "requestId": "ws1" })).await;
        assert_eq!(frames[0].get("truncated"), None);
        assert_eq!(frames[0]["payload"].as_str().map(str::len), Some(1024));
        assert_eq!(frames[1]["truncated"], true);
        assert_eq!(frames[1]["totalBytes"], 1025);
        assert_eq!(frames[1]["payload"], "한".repeat(1024));
    }

    #[tokio::test]
    async fn since와_limit로_최근_프레임만_고른다() {
        let ctx = socket_ctx();
        {
            let mut store = lock(&ctx.traffic);
            for n in 0..5_u64 {
                store.append_ws_frame("ws1", frame(WsDirection::Received, &n.to_string(), 100 + n));
            }
        }
        let payloads = |frames: Value| -> Vec<String> {
            frames
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| f["payload"].as_str().map(str::to_owned))
                .collect()
        };
        let last_two = call_json(
            &GetWsFrames,
            &ctx,
            json!({ "requestId": "ws1", "limit": 2 }),
        )
        .await;
        assert_eq!(payloads(last_two), ["3", "4"]);
        let since = call_json(
            &GetWsFrames,
            &ctx,
            json!({ "requestId": "ws1", "since": 102 }),
        )
        .await;
        assert_eq!(payloads(since), ["2", "3", "4"]);
        assert!(
            call(
                &GetWsFrames,
                &ctx,
                json!({ "requestId": "ws1", "limit": 501 })
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn 모르는_연결은_에러이고_프레임이_없는_연결은_빈_목록이다() {
        let ctx = socket_ctx();
        assert_eq!(
            call(&GetWsFrames, &ctx, json!({ "requestId": "nope" })).await,
            Err("unknown requestId: nope — use list_websockets to find valid IDs".to_owned())
        );
        assert_eq!(
            call(&GetWsFrames, &ctx, json!({ "requestId": "ws1" })).await,
            Ok("[]".to_owned())
        );
    }

    #[tokio::test]
    async fn 캡처를_켜면_초기화_스크립트를_등록하고_현재_문서에도_설치한다() {
        let browser = Arc::new(FakeBrowser {
            evaluate: Some(Ok(Value::Null)),
            ..FakeBrowser::default()
        });
        let ctx = ctx_with(browser.clone());
        assert_eq!(
            call(&WsCaptureEnable, &ctx, Value::Null).await,
            Ok("ws-capture installed; reload page to wrap pre-existing sockets".to_owned())
        );
        assert_eq!(
            browser.calls(),
            [
                format!("add:{WS_CAPTURE_SCRIPT}"),
                format!("evaluate:{WS_CAPTURE_SCRIPT}")
            ]
        );

        // Enabling again removes the script registered the first time.
        call(&WsCaptureEnable, &ctx, json!({})).await.unwrap();
        let calls = browser.calls();
        assert_eq!(calls.len(), 5);
        assert_eq!(calls[2], "remove:script-1");
        assert!(calls[3].starts_with("add:"));
    }

    #[tokio::test]
    async fn 엔진이_초기화_스크립트를_지원하지_않으면_그대로_알린다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            call(&WsCaptureEnable, &ctx, json!({})).await,
            Err(unsupported("init scripts"))
        );
    }

    #[tokio::test]
    async fn 캡처_목록은_래퍼_상태를_해석해_돌려준다() {
        let browser = FakeBrowser {
            evaluate: Some(Ok(Value::String(
                r#"[{"id":0,"url":"ws://a.test/ws","readyState":1,"openedAt":123,"protocols":null}]"#
                    .to_owned(),
            ))),
            ..FakeBrowser::default()
        };
        let ctx = ctx_with(Arc::new(browser));
        let text = call(&WsCaptureList, &ctx, json!({})).await.unwrap();
        assert!(text.contains("ws://a.test/ws"));
        // A non-string answer means the wrapper is not installed in this document.
        let browser = FakeBrowser {
            evaluate: Some(Ok(json!({}))),
            ..FakeBrowser::default()
        };
        let ctx = ctx_with(Arc::new(browser));
        assert!(call(&WsCaptureList, &ctx, json!({})).await.is_err());
    }

    #[tokio::test]
    async fn url로_보내기는_래퍼의_소켓을_찾아_보내고_상태를_그대로_전한다() {
        let browser = Arc::new(FakeBrowser {
            evaluate: Some(Ok(Value::String("ok".to_owned()))),
            ..FakeBrowser::default()
        });
        let ctx = ctx_with(browser.clone());
        let sent = call(
            &WsSend,
            &ctx,
            json!({ "url": "ws://a.test/ws", "payload": "hi" }),
        )
        .await
        .unwrap();
        assert!(sent.contains("sent on the page socket"));
        let expression = browser.calls()[0].trim_start_matches("evaluate:").to_owned();
        assert!(expression.contains(r#""ws://a.test/ws""#), "{expression}");
        assert!(expression.contains(r#""hi""#), "{expression}");

        // Closed socket without reconnect surfaces the readyState.
        let browser = Arc::new(FakeBrowser {
            evaluate: Some(Ok(Value::String("not-open:3".to_owned()))),
            ..FakeBrowser::default()
        });
        let ctx = ctx_with(browser);
        assert_eq!(
            call(
                &WsSend,
                &ctx,
                json!({ "url": "ws://a.test/ws", "payload": "hi" })
            )
            .await,
            Err("ws_send failed: not-open:3".to_owned())
        );

        // With reconnect the reopened socket reports as such.
        let browser = Arc::new(FakeBrowser {
            evaluate: Some(Ok(Value::String("reopened".to_owned()))),
            ..FakeBrowser::default()
        });
        let ctx = ctx_with(browser);
        let sent = call(
            &WsSend,
            &ctx,
            json!({ "url": "ws://a.test/ws", "payload": "hi", "reconnect": true }),
        )
        .await
        .unwrap();
        assert!(sent.contains("reopened"));
    }

    #[tokio::test]
    async fn ws_send는_인자를_검증한다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            call(&WsSend, &ctx, json!({ "payload": "hi" })).await,
            Err("pass url (preferred) or captureId".to_owned())
        );
        assert!(
            call(&WsSend, &ctx, json!({ "captureId": 0 }))
                .await
                .is_err()
        );
        assert!(
            call(&WsSend, &ctx, json!({ "captureId": -1, "payload": "x" }))
                .await
                .is_err()
        );
        assert!(
            call(&WsSend, &ctx, json!({ "captureId": 0.5, "payload": "x" }))
                .await
                .is_err()
        );
        assert!(
            call(
                &WsSend,
                &ctx,
                json!({ "captureId": 0, "payload": "x", "asBinary": "yes" })
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn capture_id로_보내면_페이지에서_표현식을_실행한다() {
        let browser = Arc::new(FakeBrowser {
            evaluate: Some(Ok(json!("ok"))),
            ..FakeBrowser::default()
        });
        let ctx = ctx_with(browser.clone());
        assert_eq!(
            call(&WsSend, &ctx, json!({ "captureId": 3, "payload": "ping" })).await,
            Ok("sent to ws#3".to_owned())
        );
        assert_eq!(
            browser.calls(),
            [format!(
                "evaluate:{}",
                build_send_expression(3, "ping", false)
            )]
        );
    }

    #[tokio::test]
    async fn 페이지가_ok가_아닌_답을_주면_실패로_알린다() {
        let answer = |value: Result<Value, String>| async move {
            let ctx = ctx_with(Arc::new(FakeBrowser {
                evaluate: Some(value),
                ..FakeBrowser::default()
            }));
            call(
                &WsSend,
                &ctx,
                json!({ "captureId": 0, "payload": "x", "asBinary": true }),
            )
            .await
        };
        assert_eq!(
            answer(Ok(json!("not-open:3"))).await,
            Err("ws_send failed: not-open:3".to_owned())
        );
        assert_eq!(
            answer(Ok(json!("no-such-capture"))).await,
            Err("ws_send failed: no-such-capture".to_owned())
        );
        assert_eq!(
            answer(Err("page crashed".to_owned())).await,
            Err("page crashed".to_owned())
        );
    }

    // ── buildSendExpression, from src/main/ws-capture.test.ts ──

    /// What the expression would return in a page holding these sockets.
    fn eval_result(expr: &str, ready_states: &[u8]) -> String {
        let index = expr
            .split_once("(window.__revWSCapture || [])[")
            .and_then(|(_, rest)| rest.split_once(']'))
            .and_then(|(index, _)| index.parse::<usize>().ok());
        match index.and_then(|index| ready_states.get(index)) {
            None => "no-such-capture".to_owned(),
            Some(1) => "ok".to_owned(),
            Some(state) => format!("not-open:{state}"),
        }
    }

    #[test]
    fn 열린_소켓이면_ok를_반환하는_표현식을_만든다_텍스트() {
        let payload = r#"{"move":[1,2]}"#;
        let expr = build_send_expression(0, payload, false);
        assert!(expr.contains("e.ws.send("));
        assert!(expr.contains(r#""{\"move\":[1,2]}""#));
        assert_eq!(eval_result(&expr, &[1]), "ok");
    }

    #[test]
    fn 페이로드의_따옴표와_특수문자는_json_stringify로_이스케이프된다() {
        let expr = build_send_expression(1, "say \"hi\" \\ & newline\n", false);
        // JSON.stringify('say "hi" \\ & newline\n')
        assert!(expr.contains(r#""say \"hi\" \\ & newline\n""#));
        assert_eq!(eval_result(&expr, &[1, 1]), "ok");
    }

    #[test]
    fn 바이너리_경로는_base64를_uint8array로_바꾸는_코드를_포함한다() {
        let expr = build_send_expression(0, "AAECAwQ=", true);
        assert!(expr.contains(r#"atob("AAECAwQ=")"#));
        assert!(expr.contains("Uint8Array"));
    }

    #[test]
    fn 닫힌_소켓이면_not_open을_반환하는_표현식이다() {
        let expr = build_send_expression(0, "x", false);
        assert_eq!(eval_result(&expr, &[3]), "not-open:3");
    }

    #[test]
    fn 없는_capture_id면_no_such_capture를_반환하는_표현식이다() {
        let expr = build_send_expression(5, "x", false);
        assert_eq!(eval_result(&expr, &[1]), "no-such-capture");
    }

    #[test]
    fn 표현식은_타입스크립트가_만드는_것과_글자까지_같다() {
        // What buildSendExpression(2, 'a"b', false) returns in node.
        assert_eq!(
            build_send_expression(2, "a\"b", false),
            "(() => {\n        const e = (window.__revWSCapture || [])[2];\n        if (!e) return 'no-such-capture';\n        if (e.ws.readyState !== 1) return 'not-open:' + e.ws.readyState;\n        e.ws.send(\"a\\\"b\");\n        return 'ok';\n      })()"
        );
        // What buildSendExpression(7, 'AAEC', true) returns in node.
        assert_eq!(
            build_send_expression(7, "AAEC", true),
            "(() => {\n        const e = (window.__revWSCapture || [])[7];\n        if (!e) return 'no-such-capture';\n        if (e.ws.readyState !== 1) return 'not-open:' + e.ws.readyState;\n        const raw = atob(\"AAEC\");\n        const buf = new Uint8Array(raw.length);\n        for (let i = 0; i < raw.length; i++) buf[i] = raw.charCodeAt(i);\n        e.ws.send(buf);\n        return 'ok';\n      })()"
        );
    }
}
