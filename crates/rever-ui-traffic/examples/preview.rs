//! Opens this crate's view alone with fixture data. See rever_ui_kit::preview.
//!
//! The page area above the panel is a blank stand-in; the drawer overlays its
//! right edge as it does in the app.
//!   REVER_PREVIEW_DETAIL=overview|headers|body|error|binary|html|long|none  what the drawer shows (default body)
//!                                                    (long: an HTML body past the size raw mode draws as one block)
//!   REVER_PREVIEW_SELECT=N                           rows ticked in the list (default 2)
//!   REVER_PREVIEW_EMPTY=1                            start with no traffic
//!   REVER_THEME, REVER_LANG                          as in the app

use std::sync::{Arc, Mutex};

use gpui::{Context, Entity, Window, div, prelude::*, px};
use rever_core::traffic::{Headers, ListFilter, RequestPatch, TrafficStore};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use rever_ui_traffic::detail_drawer::DetailDrawer;
use rever_ui_traffic::store::TrafficState;
use rever_ui_traffic::traffic_detail_drawer::Tab;
use rever_ui_traffic::traffic_list::TrafficList;

const ORDER_JSON: &str = r#"{"order":{"id":"ord_9f8a1b2c","status":"paid","total":128000,"currency":"KRW","items":[{"sku":"KB-87-BLK","name":"기계식 키보드 87키","qty":1,"price":98000,"options":{"switch":"brown","layout":"ko"}},{"sku":"CAP-PBT-01","name":"PBT 키캡 세트","qty":2,"price":15000,"options":null}],"shipping":{"method":"parcel","address":{"city":"Seoul","zip":"04524"},"tracking":null},"coupon":null,"paid":true,"ratio":0.125},"meta":{"requestId":"req_7d1e","signature":"MEUCIQDf3J8x1Yk2c0vWmQ4n7pTz9aLr5HqBn6sKd1uXo8EwYgIgT2hVb0Zc3RjPq7MnL4sXa9yKe6Wd1FuGt5HiJ0oAb8c9De2Fg","elapsedMs":34,"flags":["cache-miss","ab:checkout-v2"]}}"#;

fn headers(pairs: &[(&str, &str)]) -> Headers {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

struct Fixture {
    method: &'static str,
    url: &'static str,
    kind: &'static str,
    /// `None` leaves the request pending.
    status: Option<u16>,
    mime: &'static str,
    size: u64,
}

const FIXTURES: &[Fixture] = &[
    fx(
        "GET",
        "https://shop.example.com/",
        "Document",
        Some(200),
        "text/html",
        18_432,
    ),
    fx(
        "GET",
        "https://shop.example.com/assets/app.3f9c1a.css",
        "Stylesheet",
        Some(200),
        "text/css",
        42_110,
    ),
    fx(
        "GET",
        "https://shop.example.com/assets/vendor.8b21d0.js",
        "Script",
        Some(200),
        "application/javascript",
        1_482_204,
    ),
    fx(
        "GET",
        "https://shop.example.com/assets/app.77e0c4.js",
        "Script",
        Some(200),
        "application/javascript",
        318_770,
    ),
    fx(
        "GET",
        "https://cdn.example.com/fonts/pretendard-regular.woff2",
        "Font",
        Some(200),
        "font/woff2",
        91_204,
    ),
    fx(
        "GET",
        "https://cdn.example.com/img/hero@2x.webp",
        "Image",
        Some(200),
        "image/webp",
        240_118,
    ),
    fx(
        "GET",
        "https://api.example.com/v1/session",
        "XHR",
        Some(200),
        "application/json",
        612,
    ),
    fx(
        "POST",
        "https://api.example.com/oauth/token",
        "XHR",
        Some(200),
        "application/json",
        1_280,
    ),
    fx(
        "GET",
        "https://api.example.com/v1/users/48213/profile",
        "Fetch",
        Some(200),
        "application/json",
        2_048,
    ),
    fx(
        "GET",
        "https://api.example.com/v1/products?category=keyboard&sort=price&page=2",
        "Fetch",
        Some(200),
        "application/json",
        15_360,
    ),
    fx(
        "GET",
        "https://api.example.com/v1/products/9f8a1b2c-3d4e-5f60-7182-93a4b5c6d7e8/reviews",
        "Fetch",
        Some(304),
        "application/json",
        0,
    ),
    fx(
        "OPTIONS",
        "https://api.example.com/v1/cart/items",
        "Preflight",
        Some(204),
        "",
        0,
    ),
    fx(
        "POST",
        "https://api.example.com/v1/cart/items",
        "XHR",
        Some(201),
        "application/json",
        934,
    ),
    fx(
        "GET",
        "https://api.example.com/v1/cart",
        "XHR",
        Some(200),
        "application/json",
        3_911,
    ),
    fx(
        "GET",
        "https://shop.example.com/old-landing",
        "Document",
        Some(302),
        "text/html",
        180,
    ),
    fx(
        "GET",
        "https://api.example.com/v1/coupons/WELCOME10",
        "Fetch",
        Some(404),
        "application/json",
        143,
    ),
    fx(
        "POST",
        "https://api.example.com/v1/orders",
        "Fetch",
        Some(200),
        "application/json",
        1_024,
    ),
    fx(
        "GET",
        "https://api.example.com/v1/orders/ord_9f8a1b2c/receipt.pdf",
        "Fetch",
        Some(200),
        "application/pdf",
        88_300,
    ),
    fx(
        "POST",
        "https://api.example.com/v1/payments/authorize",
        "XHR",
        Some(500),
        "application/json",
        211,
    ),
    fx(
        "GET",
        "https://api.example.com/v1/recommendations?user=48213&limit=12",
        "Fetch",
        None,
        "",
        0,
    ),
    fx(
        "GET",
        "wss://realtime.example.com/socket?token=eyJhbGciOiJIUzI1NiJ9",
        "WebSocket",
        Some(101),
        "",
        0,
    ),
    fx(
        "POST",
        "https://telemetry.example.com/collect?v=2&tid=G-4X1",
        "Ping",
        Some(204),
        "",
        0,
    ),
    fx(
        "GET",
        "https://shop.example.com/manifest.webmanifest",
        "Manifest",
        Some(200),
        "application/manifest+json",
        512,
    ),
    fx(
        "GET",
        "https://api.example.com/v1/search/suggest?q=%ED%82%A4%EB%B3%B4%EB%93%9C",
        "XHR",
        Some(200),
        "application/json",
        5_806_080,
    ),
];

const fn fx(
    method: &'static str,
    url: &'static str,
    kind: &'static str,
    status: Option<u16>,
    mime: &'static str,
    size: u64,
) -> Fixture {
    Fixture {
        method,
        url,
        kind,
        status,
        mime,
        size,
    }
}

fn fixture_store() -> TrafficStore {
    let mut store = TrafficStore::default();
    let start = 1_763_000_000_000_u64;
    for (index, fixture) in FIXTURES.iter().enumerate() {
        let id = format!("{}.{}", 4200 + index / 6, index + 1);
        let started = start + index as u64 * 137;
        let is_json = fixture.mime == "application/json";
        store.upsert(
            RequestPatch {
                request_id: id.clone(),
                url: Some(fixture.url.into()),
                method: Some(fixture.method.into()),
                resource_type: Some(fixture.kind.into()),
                request_headers: Some(headers(&[
                    ("Accept", "application/json, text/plain, */*"),
                    ("Authorization", "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiI0ODIxMyJ9.Qm9ndXM"),
                    ("Content-Type", "application/json"),
                    ("Origin", "https://shop.example.com"),
                    ("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36"),
                ])),
                request_post_data: (fixture.method == "POST").then(|| {
                    r#"{"items":[{"sku":"KB-87-BLK","qty":1},{"sku":"CAP-PBT-01","qty":2}],"coupon":null,"gift":false}"#.into()
                }),
                ..RequestPatch::default()
            },
            started,
        );
        let Some(status) = fixture.status else {
            continue;
        };
        let is_pdf = fixture.mime == "application/pdf";
        store.upsert(
            RequestPatch {
                request_id: id,
                status: Some(status),
                mime_type: Some(fixture.mime.into()),
                completed_at: Some(started + 34 + index as u64 * 9),
                encoded_data_length: Some(fixture.size),
                response_headers: Some(headers(&[
                    ("cache-control", "no-store"),
                    ("content-type", if is_json { "application/json; charset=utf-8" } else { fixture.mime }),
                    ("set-cookie", "sid=s%3A9f8a1b2c3d4e5f60.7182; Path=/; HttpOnly; Secure; SameSite=Lax"),
                    ("x-request-id", "req_7d1e0b52c4"),
                ])),
                response_body: if is_pdf {
                    Some("JVBERi0xLjcKJcfsj6IKNSAwIG9iago8PC9MZW5ndGggNiAwIFI+PgpzdHJlYW0K".into())
                } else if status == 500 {
                    None
                } else if is_json {
                    Some(ORDER_JSON.into())
                } else {
                    Some("<!doctype html>\n<html lang=\"ko\">\n  <head><title>Shop</title></head>\n  <body>…</body>\n</html>\n".into())
                },
                response_body_base64: is_pdf.then_some(true),
                response_body_error: (status == 500)
                    .then(|| "No resource with given identifier found".into()),
                ..RequestPatch::default()
            },
            started,
        );
    }
    store
}

struct Preview {
    list: Entity<TrafficList>,
    drawer: Entity<DetailDrawer>,
    theme_mode: ThemeMode,
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.theme_mode, window.appearance());
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.0))
            .font_family(".SystemUIFont")
            .child(div().flex_1().min_h_0().bg(theme.viewport_bg))
            .child(
                div()
                    .h(px(380.0))
                    .flex_none()
                    .border_t_1()
                    .border_color(theme.border_2)
                    .child(self.list.clone()),
            )
            .child(self.drawer.clone())
    }
}

fn main() {
    let env = |name: &str| std::env::var(name).unwrap_or_default();
    rever_ui_kit::preview::run(1180.0, 780.0, move |_, cx| {
        let (lang, theme_mode) = (Lang::detect(), ThemeMode::from_env());
        let mut store = if env("REVER_PREVIEW_EMPTY").is_empty() {
            fixture_store()
        } else {
            TrafficStore::default()
        };
        if env("REVER_PREVIEW_DETAIL") == "long" {
            let page = store.list(&ListFilter {
                limit: Some(500),
                ..ListFilter::default()
            });
            let id = page.iter().find(|r| r.url.ends_with("/old-landing"));
            if let Some(request_id) = id.map(|r| r.request_id.clone()) {
                let lines: Vec<String> = (1..=6_000)
                    .map(|n| {
                        format!("    <li data-row=\"{n}\">상품 {n} — keyboard, keycap, switch</li>")
                    })
                    .collect();
                let body = format!("<!doctype html>\n<ul>\n{}\n</ul>\n", lines.join("\n"));
                store.upsert(
                    RequestPatch {
                        request_id,
                        response_body: Some(body),
                        ..RequestPatch::default()
                    },
                    0,
                );
            }
        }
        let store = Arc::new(Mutex::new(store));
        let state = cx.new(|_| TrafficState::default());
        let list =
            cx.new(|cx| TrafficList::new(store.clone(), state.clone(), lang, theme_mode, cx));

        let detail = env("REVER_PREVIEW_DETAIL");
        let select = env("REVER_PREVIEW_SELECT").parse().unwrap_or(2);
        state.update(cx, |state, cx| {
            let ids: Vec<String> = state
                .order
                .iter()
                .filter(|id| {
                    let kind = state.entries.get(*id).map(|e| e.resource_type.as_str());
                    matches!(kind, Some("XHR" | "Fetch"))
                })
                .cloned()
                .collect();
            for id in ids.iter().skip(2).take(select) {
                state.toggle_select(id);
            }
            // Which request to open: the failed one, the PDF, or the order with a JSON body.
            let wanted = match detail.as_str() {
                "none" => None,
                "error" => Some("/payments/authorize"),
                "binary" => Some("receipt.pdf"),
                "html" | "long" => Some("/old-landing"),
                _ => Some("/v1/orders"),
            };
            let open = wanted.and_then(|needle| {
                let found = state.entries.values().find(|e| e.url.ends_with(needle));
                found.map(|e| e.request_id.clone())
            });
            if let Some(id) = open {
                state.open_detail(&id);
            }
            cx.notify();
        });
        let tab = match detail.as_str() {
            "overview" => Tab::Overview,
            "headers" => Tab::Headers,
            _ => Tab::Body,
        };
        // Made after the detail is open, so the drawer is there on the first frame:
        // a preview window behind other windows gets no animation frames.
        let drawer = cx
            .new(|cx| DetailDrawer::new(store.clone(), state.clone(), None, lang, theme_mode, cx));
        let content = drawer.read(cx).content().cloned();
        if let Some(content) = content {
            content.update(cx, |content, cx| content.set_tab(tab, cx));
        }

        cx.new(|_| Preview {
            list,
            drawer,
            theme_mode,
        })
    });
}
