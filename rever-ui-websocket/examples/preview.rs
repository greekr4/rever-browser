//! Opens this crate's view alone with fixture data. See rever_ui_kit::preview.
//!
//! `REVER_PREVIEW_PAYLOAD=text` starts with that text in the composer (`\n` breaks lines).
//! `REVER_PREVIEW_EMPTY=1` starts without any connection.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::AppContext;
use rever_core::traffic::{RequestPatch, TrafficStore, WsDirection, WsFrame};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::ThemeMode;
use rever_ui_websocket::{LiveSocket, SendOutcome, SendRequest, WebSocketPanel, WsHost};

const ORDERS: &str = "wss://stream.example.com/realtime?channel=orders&token=eyJhbGciOiJIUzI1NiJ9";
const CHAT: &str = "wss://stream.example.com/realtime?channel=chat";
const GRAPHQL: &str = "wss://api.example.org:8443/graphql";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

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

/// A page that keeps two of the three sockets open and echoes what is sent.
struct FakePage {
    store: Arc<Mutex<TrafficStore>>,
    reopened: AtomicU64,
}

impl WsHost for FakePage {
    fn sockets(&self) -> Result<Vec<LiveSocket>, String> {
        let socket = |url: &str, ready_state, frame: &str| LiveSocket {
            url: url.to_owned(),
            ready_state,
            frame: frame.to_owned(),
        };
        Ok(vec![
            socket(ORDERS, 1, "page"),
            socket(CHAT, 1, "https://widget.example.com"),
            socket(GRAPHQL, 3, "page"),
        ])
    }

    fn send(&self, request: SendRequest) -> SendOutcome {
        std::thread::sleep(Duration::from_millis(400));
        let mut store = self.store.lock().unwrap();
        let now = now_ms();
        let id = if request.reconnect {
            let id = format!("reopened-{}", self.reopened.fetch_add(1, Ordering::Relaxed));
            open(&mut store, &id, &request.url, now);
            id
        } else if request.url == ORDERS {
            "ws-orders".to_owned()
        } else {
            "ws-chat".to_owned()
        };
        let opcode = if request.as_binary { 2 } else { 1 };
        frame(&mut store, &id, true, opcode, &request.payload, now);
        frame(
            &mut store,
            &id,
            false,
            1,
            r#"{"type":"ack","ok":true}"#,
            now + 1,
        );
        SendOutcome {
            ok: true,
            error: None,
            reconnected: request.reconnect,
        }
    }
}

fn fixture() -> TrafficStore {
    let mut store = TrafficStore::default();
    let start = now_ms() - 60_000;
    open(&mut store, "ws-graphql", GRAPHQL, start);
    open(&mut store, "ws-chat", CHAT, start + 1_000);
    open(&mut store, "ws-orders", ORDERS, start + 2_000);

    frame(
        &mut store,
        "ws-graphql",
        true,
        1,
        r#"{"type":"connection_init","payload":{}}"#,
        start + 100,
    );
    frame(
        &mut store,
        "ws-graphql",
        false,
        1,
        r#"{"type":"connection_ack"}"#,
        start + 180,
    );

    frame(
        &mut store,
        "ws-chat",
        true,
        1,
        "SUBSCRIBE\nid:sub-0\ndestination:/topic/support.room.42\n\n\0",
        start + 1_200,
    );
    frame(
        &mut store,
        "ws-chat",
        false,
        1,
        "MESSAGE\nsubscription:sub-0\ncontent-type:text/plain\n\n안녕하세요, 무엇을 도와드릴까요?\0",
        start + 1_900,
    );

    let t = start + 2_100;
    frame(
        &mut store,
        "ws-orders",
        true,
        1,
        r#"{"op":"subscribe","channel":"orders","symbols":["BTC-KRW","ETH-KRW"],"depth":20}"#,
        t,
    );
    frame(
        &mut store,
        "ws-orders",
        false,
        1,
        r#"{"op":"subscribed","channel":"orders","id":"c0a8-01","serverTime":1794000000000}"#,
        t + 40,
    );
    frame(
        &mut store,
        "ws-orders",
        false,
        1,
        r#"{"channel":"orders","symbol":"BTC-KRW","bids":[[152340000,0.4132],[152339000,1.2]],"asks":[[152341000,0.08],[152345000,2.5]],"seq":88120431}"#,
        t + 300,
    );
    frame(
        &mut store,
        "ws-orders",
        true,
        1,
        r#"{"op":"ping","ts":1794000001000}"#,
        t + 5_000,
    );
    frame(
        &mut store,
        "ws-orders",
        false,
        1,
        r#"{"op":"pong","ts":1794000001004}"#,
        t + 5_004,
    );
    frame(
        &mut store,
        "ws-orders",
        true,
        1,
        "SEND\ndestination:/app/orders.cancel\ncontent-type:application/json\n\n{\"orderId\":\"7f3a\"}\0",
        t + 6_000,
    );
    frame(
        &mut store,
        "ws-orders",
        false,
        2,
        "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
        t + 6_200,
    );
    let snapshot: String = (0..90)
        .map(|level| {
            format!(
                "[{},{}.{:04}],",
                152_300_000 + level * 1_000,
                level % 7,
                level * 37
            )
        })
        .collect();
    frame(
        &mut store,
        "ws-orders",
        false,
        1,
        &format!(r#"{{"channel":"orders","type":"snapshot","levels":[{snapshot}]}}"#),
        t + 7_000,
    );
    frame(
        &mut store,
        "ws-orders",
        true,
        1,
        r#"{"op":"unsubscribe","channel":"orders","symbols":["ETH-KRW"]}"#,
        t + 9_000,
    );
    frame(
        &mut store,
        "ws-orders",
        false,
        1,
        r#"{"op":"unsubscribed","symbols":["ETH-KRW"]}"#,
        t + 9_030,
    );
    store
}

fn main() {
    let empty = std::env::var_os("REVER_PREVIEW_EMPTY").is_some();
    let store = Arc::new(Mutex::new(if empty {
        TrafficStore::default()
    } else {
        fixture()
    }));
    let host = Arc::new(FakePage {
        store: store.clone(),
        reopened: AtomicU64::new(0),
    });
    rever_ui_kit::preview::run(1020.0, 320.0, move |_window, cx| {
        cx.new(|cx| {
            let mut panel =
                WebSocketPanel::new(store, host, Lang::detect(), ThemeMode::from_env(), cx);
            if let Ok(payload) = std::env::var("REVER_PREVIEW_PAYLOAD") {
                panel.set_payload(payload.replace("\\n", "\n"), cx);
            }
            panel
        })
    });
}
