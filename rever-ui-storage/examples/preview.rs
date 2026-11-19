//! Opens this crate's view alone with fixture data. See rever_ui_kit::preview.
//!
//! The panel runs against the in-memory host, so every edit really lands. What is open
//! at start is picked with `REVER_PREVIEW_STATE`: `cookies` (default), `local`,
//! `session`, `editing`, `adding`, `editor`, `new`, `samesite`, `browsers`, `imported`,
//! `empty`.

use std::rc::Rc;
use std::time::Duration;

use gpui::{Context, Entity, Window, div, prelude::*, px};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use rever_ui_storage::cookies_panel::{
    BrowserInfo, BrowserProfileInfo, CookieRow, CookiesPanel, DialogRecord, DialogSettings,
    Persistence, Section,
};
use rever_ui_storage::memory_host::{MemoryState, MemoryStorageHost};

fn cookie(name: &str, value: &str, domain: &str, expires: Option<f64>, flags: &str) -> CookieRow {
    CookieRow {
        name: name.into(),
        value: value.into(),
        domain: domain.into(),
        path: "/".into(),
        expires,
        secure: flags.contains('s').then_some(true),
        http_only: flags.contains('h').then_some(true),
        same_site: match () {
            _ if flags.contains('L') => Some("Lax".into()),
            _ if flags.contains('S') => Some("Strict".into()),
            _ if flags.contains('N') => Some("None".into()),
            _ => None,
        },
    }
}

fn pairs(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn fixture(empty: bool) -> MemoryState {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_millis() as f64);
    let browser = |id: &str, name: &str, profiles: &[(&str, &str)]| BrowserInfo {
        id: id.into(),
        name: name.into(),
        profiles: profiles
            .iter()
            .map(|(id, name)| BrowserProfileInfo {
                id: id.to_string(),
                name: name.to_string(),
            })
            .collect(),
    };
    let dialog = |age: f64, kind: &str, message: &str| DialogRecord {
        ts: now - age,
        kind: kind.into(),
        message: message.into(),
        url: "https://github.com/settings".into(),
    };
    let mut state = MemoryState {
        origin: Some("https://github.com".into()),
        persistence: Persistence {
            enabled: true,
            snapshot_count: 3,
        },
        browsers: vec![
            browser(
                "chrome",
                "Google Chrome",
                &[("Default", "Person 1"), ("Profile 1", "Work")],
            ),
            browser("arc", "Arc", &[("Default", "Default")]),
            browser(
                "firefox",
                "Firefox",
                &[("abcd.default-release", "default-release")],
            ),
        ],
        importable: vec![
            (
                "chrome".into(),
                cookie(
                    "dotcom_user",
                    "greekr4",
                    ".github.com",
                    Some(1_830_000_000.0),
                    "shL",
                ),
            ),
            (
                "chrome".into(),
                cookie(
                    "NID",
                    "511=abc",
                    ".google.com",
                    Some(1_800_000_000.0),
                    "shN",
                ),
            ),
        ],
        ..MemoryState::default()
    };
    if empty {
        return state;
    }
    state.cookies = vec![
        // CDP reports a session cookie's expiry as -1.
        cookie(
            "_gh_sess",
            "Zk9oT2VQb3NLQ0xXc3l2d0ZxN0V5aXJ5c1Z0M2k4bHNpSnB6Q2JkYlFzb1VqT0x6eTVrRXJ2bUVqZ3dWc0JtQ1hXQ2ZtWGpXb3hYQ0ZqV1E9PS0tYjJkM2Y0",
            "github.com",
            Some(-1.0),
            "shL",
        ),
        cookie(
            "_octo",
            "GH1.1.1234567890.1731400000",
            ".github.com",
            Some(1_762_936_000.0),
            "sL",
        ),
        cookie(
            "logged_in",
            "yes",
            ".github.com",
            Some(1_762_936_000.0),
            "shL",
        ),
        cookie(
            "color_mode",
            "%7B%22color_mode%22%3A%22auto%22%7D",
            ".github.com",
            None,
            "sL",
        ),
        cookie("tz", "Asia%2FSeoul", ".github.com", Some(-1.0), "sL"),
        cookie(
            "preferred_color_mode",
            "dark",
            ".github.com",
            Some(1_793_000_000.0),
            "S",
        ),
        cookie(
            "__Host-user_session_same_site",
            "aB3dE5fG7hI9jK1lM3nO5pQ7rS9tU1vW3xY5z",
            "github.com",
            Some(1_763_200_000.0),
            "shS",
        ),
    ];
    state.local = pairs(&[
        ("theme", "dark"),
        (
            "ref-selector:greekr4/rever-browser:branch",
            "{\"refs\":[\"main\",\"spike/servo-shell\",\"tauri-archive\"],\"cacheKey\":\"v0:1731400000.0\"}",
        ),
        (
            "2",
            "an integer-like key sorts first, as in a JavaScript object",
        ),
        ("soft-nav:marker", "pjax"),
        (
            "jump_to:page_views",
            "{\"greekr4/rever-browser\":{\"lastVisitedAt\":1731400000,\"visitCount\":42},\"zed-industries/zed\":{\"lastVisitedAt\":1731300000,\"visitCount\":7}}",
        ),
        ("한글 키", "값도 한글"),
    ]);
    state.session = pairs(&[
        ("soft-nav:referrer", "https://github.com/greekr4"),
        ("stats:session-id", "5f1c2a9e-7b43-4d0c-9a52-0f6e1c3b8d77"),
    ]);
    state.dialog = DialogSettings {
        auto_dismiss: true,
        history: vec![
            dialog(95_000.0, "alert", "Your session has expired."),
            dialog(
                41_000.0,
                "confirm",
                "Leave site? Changes you made may not be saved.",
            ),
            dialog(8_000.0, "prompt", "Enter the repository name to confirm"),
            dialog(2_000.0, "beforeunload", ""),
        ],
    };
    state
}

struct Preview {
    panel: Entity<CookiesPanel>,
    theme_mode: ThemeMode,
}

impl Preview {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = std::env::var("REVER_PREVIEW_STATE").unwrap_or_default();
        let host = Rc::new(MemoryStorageHost::new(fixture(state == "empty")));
        let (lang, theme_mode) = (Lang::detect(), ThemeMode::from_env());
        let panel = cx.new(|cx| CookiesPanel::new(host, lang, theme_mode, cx));
        // After the first answers from the host: the rows have to be there to open one.
        let opened = panel.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let _ = opened.update_in(cx, |panel, window, cx| match state.as_str() {
                "local" => panel.set_section(Section::Local, cx),
                "session" => panel.set_section(Section::Session, cx),
                "editing" => {
                    panel.set_section(Section::Local, cx);
                    panel.start_editing("theme", window, cx);
                }
                "adding" => {
                    panel.set_section(Section::Local, cx);
                    panel.start_adding(cx);
                }
                "editor" => panel.edit_cookie(0, cx),
                "new" => panel.add_cookie(cx),
                "samesite" => {
                    panel.edit_cookie(0, cx);
                    panel.show_select("samesite", cx);
                }
                "browsers" => panel.show_select("browser", cx),
                "imported" => panel.run_import(cx),
                _ => {}
            });
        })
        .detach();
        Self { panel, theme_mode }
    }
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.theme_mode, window.appearance());
        div()
            .size_full()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.0))
            .font_family(".SystemUIFont")
            .child(self.panel.clone())
    }
}

fn main() {
    rever_ui_kit::preview::run(900.0, 620.0, |window, cx| {
        cx.new(|cx| Preview::new(window, cx))
    });
}
