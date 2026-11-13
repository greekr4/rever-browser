//! Opens this crate's view alone with fixture data. See rever_ui_kit::preview.
//!
//! The line at the bottom shows the last request the panel asked the host to open.
//!   REVER_PREVIEW_DOMAIN=naver.co.kr       the domain picked in the select (default: the busiest)
//!   REVER_PREVIEW_FOCUS=api.example.com    the host drilled into (default: the ring of hosts)
//!   REVER_PREVIEW_MENU=1                   start with the domain list open
//!   REVER_PREVIEW_EMPTY=1                  start with no traffic
//!   REVER_THEME, REVER_LANG                as in the app

use std::sync::{Arc, Mutex};

use gpui::{Context, Entity, SharedString, Window, div, prelude::*, px};
use rever_core::traffic::{RequestPatch, TrafficStore};
use rever_ui_apimap::api_map_panel::{ApiMapEvent, ApiMapPanel};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};

/// (method, URL, status, how many times it was called)
const CALLS: &[(&str, &str, u16, usize)] = &[
    ("GET", "https://api.example.com/v1/products", 200, 14),
    ("GET", "https://api.example.com/v1/products/48213", 200, 9),
    ("GET", "https://api.example.com/v1/products/48214", 404, 1),
    ("POST", "https://api.example.com/v1/cart/items", 201, 6),
    ("DELETE", "https://api.example.com/v1/cart/items/7", 204, 2),
    ("PUT", "https://api.example.com/v1/users/me/address", 200, 1),
    ("PATCH", "https://api.example.com/v1/users/me", 200, 2),
    ("POST", "https://api.example.com/v1/orders", 201, 3),
    (
        "GET",
        "https://api.example.com/v1/orders/9f8a1b2c-3d4e-5f60-7182-93a4b5c6d7e8",
        200,
        4,
    ),
    (
        "GET",
        "https://api.example.com/v1/recommendations/personalized/for-you/today",
        200,
        5,
    ),
    ("OPTIONS", "https://api.example.com/v1/orders", 204, 3),
    (
        "POST",
        "https://api.example.com/v1/payments/authorize",
        500,
        1,
    ),
    ("POST", "https://auth.example.com/oauth/token", 200, 4),
    ("POST", "https://auth.example.com/session/refresh", 401, 2),
    ("GET", "https://auth.example.com/v1/me", 200, 3),
    ("GET", "https://search.example.com/suggest", 200, 22),
    ("GET", "https://search.example.com/query", 200, 7),
    ("POST", "https://metrics.example.com/collect", 204, 31),
    ("GET", "https://example.com/config.json", 200, 1),
    (
        "GET",
        "https://cdn.assets.example.com/manifest.json",
        200,
        1,
    ),
    ("GET", "https://mail.naver.co.kr/json/list", 200, 8),
    ("POST", "https://mail.naver.co.kr/json/read", 200, 5),
    ("POST", "https://nid.naver.co.kr/nidlogin.login", 302, 1),
    ("GET", "http://localhost:8777/api/fixtures", 200, 2),
    ("GET", "http://localhost:8777/api/fixtures/12", 200, 2),
    ("POST", "http://localhost:8777/api/login", 200, 1),
];

fn fixture_store() -> TrafficStore {
    let mut store = TrafficStore::default();
    let start = 1_763_000_000_000_u64;
    let mut index = 0_u64;
    for (method, url, status, times) in CALLS {
        for _ in 0..*times {
            index += 1;
            let patch = RequestPatch {
                request_id: format!("4200.{index}"),
                url: Some((*url).into()),
                method: Some((*method).into()),
                resource_type: Some(
                    if index.is_multiple_of(3) {
                        "XHR"
                    } else {
                        "Fetch"
                    }
                    .into(),
                ),
                status: Some(*status),
                ..RequestPatch::default()
            };
            store.upsert(patch, start + index * 137);
        }
    }
    // Not an API call: the map must leave it out.
    let page = RequestPatch {
        request_id: "4200.0".into(),
        url: Some("https://www.example.com/".into()),
        method: Some("GET".into()),
        resource_type: Some("Document".into()),
        status: Some(200),
        ..RequestPatch::default()
    };
    store.upsert(page, start);
    store
}

struct Preview {
    panel: Entity<ApiMapPanel>,
    theme_mode: ThemeMode,
    last_request: Option<SharedString>,
}

impl Preview {
    fn new(cx: &mut Context<Self>) -> Self {
        let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let store = if env("REVER_PREVIEW_EMPTY").is_some() {
            TrafficStore::default()
        } else {
            fixture_store()
        };
        let (lang, theme_mode) = (Lang::detect(), ThemeMode::from_env());
        let panel = cx.new(|cx| {
            let mut panel = ApiMapPanel::new(Arc::new(Mutex::new(store)), lang, theme_mode, cx);
            if let Some(domain) = env("REVER_PREVIEW_DOMAIN") {
                panel.select_domain(&domain, cx);
            }
            panel.set_focus(env("REVER_PREVIEW_FOCUS"), cx);
            if env("REVER_PREVIEW_MENU").is_some() {
                panel.toggle_menu(cx);
            }
            panel
        });
        cx.subscribe(&panel, |this, _, event: &ApiMapEvent, cx| {
            let ApiMapEvent::OpenRequest(id) = event;
            this.last_request = Some(id.clone().into());
            cx.notify();
        })
        .detach();
        Self {
            panel,
            theme_mode,
            last_request: None,
        }
    }
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.theme_mode, window.appearance());
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.0))
            .font_family(".SystemUIFont")
            .child(div().flex_1().min_h_0().child(self.panel.clone()))
            .child(
                div()
                    .flex_none()
                    .px(px(10.0))
                    .pb(px(6.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_dim)
                    .child(match &self.last_request {
                        Some(id) => format!("OpenRequest → {id}"),
                        None => "OpenRequest → (click an endpoint)".to_string(),
                    }),
            )
    }
}

fn main() {
    rever_ui_kit::preview::run(900.0, 520.0, |_, cx| cx.new(Preview::new));
}
