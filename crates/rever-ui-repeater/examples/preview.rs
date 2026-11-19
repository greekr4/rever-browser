//! Opens this crate's two panels in one window with fixture data: the HTTP repeater on
//! top, the WebSocket repeater underneath. See rever_ui_kit::preview.
//!
//! `REVER_PREVIEW_EMPTY=1` starts both panels with nothing loaded.
//! `REVER_PREVIEW_IDLE=1` loads the fixtures but sends nothing, so the panels show what
//! they look like before the first send.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div, px,
};
use rever_core::traffic::{RequestPatch, StoredRequest, TrafficStore, WsDirection, WsFrame};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use rever_ui_repeater::{
    RepeaterPanel, RepeaterRequestSpec, RepeaterResponse, RepeaterSender, RepeaterStore,
    WsRepeaterPanel,
};
use rever_ui_websocket::{LiveSocket, SendOutcome, SendRequest, WsHost};

const ORDERS: &str = "wss://stream.example.com/realtime?channel=orders&token=eyJhbGciOiJIUzI1NiJ9";
const CHAT: &str = "wss://stream.example.com/realtime?channel=chat";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── HTTP repeater ───────────────────────────────────────────────────────────

fn captured_request() -> StoredRequest {
    StoredRequest {
        request_id: "req-42".into(),
        url: "https://api.example.com/v2/orders?limit=20&cursor=eyJvIjoxMjB9".into(),
        method: "POST".into(),
        request_headers: Some(
            [
                ("accept", "application/json, text/plain, */*"),
                (
                    "authorization",
                    "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiI0MiJ9.c2lnbmF0dXJl",
                ),
                ("content-type", "application/json"),
                ("x-client-version", "4.18.0"),
                ("x-request-id", "7f3a9c1e-5b2d-4e8f-a1c6-0d9e8b7a6f54"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
        ),
        request_post_data: Some(
            "{\n  \"symbol\": \"BTC-KRW\",\n  \"side\": \"buy\",\n  \"price\": 152340000,\n  \"quantity\": 0.0125,\n  \"note\": \"한글 메모\"\n}"
                .into(),
        ),
        ..StoredRequest::default()
    }
}

fn response(status: u32, status_text: &str, body: &str, time_ms: u64) -> RepeaterResponse {
    RepeaterResponse {
        status,
        status_text: status_text.into(),
        headers: [
            ("content-type", "application/json; charset=utf-8"),
            ("cache-control", "no-store"),
            ("x-ratelimit-remaining", "58"),
            ("date", "Fri, 13 Nov 2026 02:14:07 GMT"),
        ]
        .into_iter()
        .collect(),
        body: body.into(),
        body_truncated: false,
        body_byte_length: body.len() as u64,
        time_ms,
        error: None,
    }
}

const ORDER_BODY: &str = r#"{"id":"ord_9f2c","symbol":"BTC-KRW","side":"buy","status":"accepted","price":152340000,"quantity":0.0125,"fills":[],"fees":{"maker":0.0004,"taker":0.0005},"createdAt":"2026-11-13T02:14:07.412Z","links":{"self":"/v2/orders/ord_9f2c","cancel":"/v2/orders/ord_9f2c/cancel"}}"#;

/// Answers every send with the same accepted order, after a short wait.
struct FakeSender;

impl RepeaterSender for FakeSender {
    fn send_raw(&self, _spec: RepeaterRequestSpec) -> Result<RepeaterResponse, String> {
        std::thread::sleep(Duration::from_millis(400));
        Ok(response(201, "Created", ORDER_BODY, 412))
    }
}

/// A store with the request loaded and, unless `idle`, three sends already answered.
fn repeater_store(empty: bool, idle: bool) -> RepeaterStore {
    let mut store = RepeaterStore::new();
    if empty {
        return store;
    }
    store.load_from_traffic("req-42", Some(&captured_request()));
    if idle {
        return store;
    }
    let answers = [
        Ok(response(
            401,
            "Unauthorized",
            r#"{"error":"token_expired"}"#,
            88,
        )),
        Ok(RepeaterResponse {
            error: Some("Failed to fetch".into()),
            time_ms: 15_003,
            ..RepeaterResponse::default()
        }),
        Ok(response(201, "Created", ORDER_BODY, 412)),
    ];
    for (index, answer) in answers.into_iter().enumerate() {
        if let Some(request) = store.begin_send() {
            store.finish_send(request, answer, now_ms() + index as u64);
        }
    }
    store
}

// ── WebSocket repeater ──────────────────────────────────────────────────────

fn open(store: &mut TrafficStore, id: &str, url: &str, started_at: u64) {
    store.upsert(
        RequestPatch {
            request_id: id.to_owned(),
            url: Some(url.to_owned()),
            method: Some("GET".to_owned()),
            resource_type: Some("WebSocket".to_owned()),
            started_at: Some(started_at),
            status: Some(101),
            ..RequestPatch::default()
        },
        started_at,
    );
}

fn frame(store: &mut TrafficStore, id: &str, sent: bool, opcode: u8, payload: &str, ts: u64) {
    store.append_ws_frame(
        id,
        WsFrame {
            direction: if sent {
                WsDirection::Sent
            } else {
                WsDirection::Received
            },
            opcode,
            payload_data: payload.to_owned(),
            timestamp: ts,
            mask: sent.then_some(true),
        },
    );
}

fn traffic() -> TrafficStore {
    let mut store = TrafficStore::default();
    let start = now_ms() - 60_000;
    open(&mut store, "ws-chat", CHAT, start);
    open(&mut store, "ws-orders", ORDERS, start + 1_000);
    frame(
        &mut store,
        "ws-chat",
        true,
        1,
        "SUBSCRIBE\nid:sub-0\ndestination:/topic/support.room.42\n\n\0",
        start + 200,
    );
    let t = start + 1_100;
    frame(
        &mut store,
        "ws-orders",
        true,
        1,
        r#"{"op":"auth","token":"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiI0MiJ9.c2ln"}"#,
        t,
    );
    frame(
        &mut store,
        "ws-orders",
        false,
        1,
        r#"{"op":"auth","ok":true}"#,
        t + 30,
    );
    frame(
        &mut store,
        "ws-orders",
        true,
        1,
        r#"{"op":"subscribe","channel":"orders","symbols":["BTC-KRW","ETH-KRW"],"depth":20,"snapshot":true,"clientId":"web-4.18.0"}"#,
        t + 100,
    );
    frame(
        &mut store,
        "ws-orders",
        true,
        2,
        "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
        t + 2_000,
    );
    frame(
        &mut store,
        "ws-orders",
        true,
        1,
        "SEND\ndestination:/app/orders.cancel\ncontent-type:application/json\n\n{\"orderId\":\"7f3a\"}\0",
        t + 3_000,
    );
    frame(
        &mut store,
        "ws-orders",
        true,
        1,
        r#"{"op":"ping","ts":1794000001000}"#,
        t + 5_000,
    );
    store
}

/// A page that keeps the orders socket open and answers each send with two frames.
struct FakePage {
    store: Arc<Mutex<TrafficStore>>,
}

impl WsHost for FakePage {
    fn sockets(&self) -> Result<Vec<LiveSocket>, String> {
        Ok(vec![LiveSocket {
            url: ORDERS.to_owned(),
            ready_state: 1,
            frame: "page".to_owned(),
        }])
    }

    fn send(&self, request: SendRequest) -> SendOutcome {
        std::thread::sleep(Duration::from_millis(120));
        let mut store = self.store.lock().unwrap();
        let now = now_ms();
        let id = if request.url == ORDERS {
            "ws-orders"
        } else {
            "ws-chat"
        };
        let opcode = if request.as_binary { 2 } else { 1 };
        frame(&mut store, id, true, opcode, &request.payload, now);
        frame(
            &mut store,
            id,
            false,
            1,
            r#"{"op":"subscribed","channel":"orders","id":"c0a8-01"}"#,
            now + 38,
        );
        frame(
            &mut store,
            id,
            false,
            1,
            r#"{"channel":"orders","symbol":"BTC-KRW","bids":[[152340000,0.4132],[152339000,1.2]],"asks":[[152341000,0.08],[152345000,2.5]],"seq":88120431}"#,
            now + 215,
        );
        SendOutcome {
            ok: true,
            error: None,
            reconnected: request.reconnect,
        }
    }
}

struct Preview {
    http: Entity<RepeaterPanel>,
    ws: Entity<WsRepeaterPanel>,
    theme_mode: ThemeMode,
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.theme_mode, window.appearance());
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .child(div().h(px(540.0)).flex_none().child(self.http.clone()))
            .child(div().h(px(6.0)).flex_none().bg(theme.border_2))
            .child(div().flex_1().min_h_0().child(self.ws.clone()))
    }
}

fn main() {
    let empty = std::env::var_os("REVER_PREVIEW_EMPTY").is_some();
    let idle = std::env::var_os("REVER_PREVIEW_IDLE").is_some();
    let traffic = Arc::new(Mutex::new(if empty {
        TrafficStore::default()
    } else {
        traffic()
    }));
    let page = Arc::new(FakePage {
        store: traffic.clone(),
    });
    let (lang, theme_mode) = (Lang::detect(), ThemeMode::from_env());
    rever_ui_kit::preview::run(1180.0, 900.0, move |_window, cx| {
        let store = cx.new(|_| repeater_store(empty, idle));
        let http =
            cx.new(|cx| RepeaterPanel::new(store, Arc::new(FakeSender), lang, theme_mode, cx));
        let ws = cx.new(|cx| {
            let panel = WsRepeaterPanel::new(traffic, page, lang, theme_mode, cx);
            if !empty && !idle {
                // Once the page's open sockets are known: the subscribe frame becomes
                // the original, two steps go in the sequence, and one send goes out.
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(300))
                        .await;
                    let _ = this.update(cx, |panel: &mut WsRepeaterPanel, cx| {
                        panel.load_sent_frame(3, cx);
                        panel.add_step(cx);
                        panel.load_sent_frame(0, cx);
                        panel.add_step(cx);
                        panel.load_sent_frame(3, cx);
                        panel.send(cx);
                    });
                })
                .detach();
            }
            panel
        });
        cx.new(|_| Preview {
            http,
            ws,
            theme_mode,
        })
    });
}
