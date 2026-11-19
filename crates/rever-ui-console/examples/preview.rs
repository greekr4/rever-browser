//! Opens this crate's views alone with fixture data. See rever_ui_kit::preview.
//!
//! `REVER_THEME=dark` and `REVER_LANG=ko` switch theme and language;
//! `REVER_PREVIEW_EMPTY=1` shows both panels with nothing logged.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{Context, Entity, Window, div, prelude::*, px};
use rever_core::traffic::{ConsoleEntry, RuntimeException, TrafficStore};
use rever_ui_console::console_panel::ConsolePanel;
use rever_ui_console::exceptions_panel::ExceptionsPanel;
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use serde_json::json;

struct Preview {
    console: Entity<ConsolePanel>,
    exceptions: Entity<ExceptionsPanel>,
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
            .text_color(theme.text)
            .text_size(px(13.0))
            .font_family(".SystemUIFont")
            .child(div().flex_1().min_h_0().child(self.console.clone()))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_color(theme.border_2)
                    .child(self.exceptions.clone()),
            )
    }
}

fn fixtures(store: &mut TrafficStore) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);
    let console = [
        ("log", "[vite] connected."),
        (
            "info",
            "Download the React DevTools for a better development experience: https://react.dev/link/react-devtools",
        ),
        ("debug", "router: matched /dashboard/[team]/usage"),
        (
            "warning",
            "[Deprecation] Synchronous XMLHttpRequest on the main thread is deprecated because of its detrimental effects to the end user's experience.",
        ),
        (
            "error",
            "Failed to load resource: the server responded with a status of 401 ()",
        ),
        (
            "log",
            "session {\n  user: \"tk\",\n  plan: \"pro\",\n  expiresAt: 1763020800\n}",
        ),
        ("command", "document.querySelectorAll('script[src]').length"),
        ("result", "42"),
        ("command", "JSON.parse(localStorage.token)"),
        (
            "error",
            "Uncaught SyntaxError: Unexpected token 'e', \"eyJhbGciOi\"... is not valid JSON",
        ),
        (
            "log",
            "x-signature=9f2c41d07be35a8846c1e0f3ad72b95c1b0e6d4f7a3829c5d1e0b6f4a7c3d2e19f2c41d07be35a8846c1e0f3ad72b95c1b0e6d4f7a3829c5d1e0b6f4a7c3d2e1",
        ),
        ("table", "한글 로그도 그대로 보인다"),
    ];
    for (index, (kind, text)) in console.into_iter().enumerate() {
        store.append_console(ConsoleEntry {
            ts: now - 60_000 + index as u64 * 1_500,
            kind: kind.into(),
            text: text.into(),
            ..ConsoleEntry::default()
        });
    }
    let frame = |name: &str, line: u32| {
        json!({
            "functionName": name,
            "scriptId": "118",
            "url": "https://app.example.com/assets/index-D4f8a1c2.js",
            "lineNumber": line,
            "columnNumber": 28114
        })
    };
    let exceptions = [
        (
            "Uncaught TypeError: Cannot read properties of undefined (reading 'token')",
            Some(json!({ "callFrames": [frame("signRequest", 41), frame("fetchUsage", 97)] })),
        ),
        (
            "Uncaught (in promise) Error: Request failed with status code 401",
            None,
        ),
        (
            "Uncaught ReferenceError: gtag is not defined at https://app.example.com/assets/vendor-B7e19f03.js:2:118334 while handling the route change to /dashboard/usage",
            Some(json!({ "callFrames": [frame("", 2)] })),
        ),
    ];
    for (index, (text, stack_trace)) in exceptions.into_iter().enumerate() {
        store.append_exception(RuntimeException {
            ts: now - 30_000 + index as u64 * 4_000,
            text: text.into(),
            stack_trace,
            ..RuntimeException::default()
        });
    }
}

fn main() {
    rever_ui_kit::preview::run(760.0, 640.0, |_, cx| {
        let mut store = TrafficStore::default();
        let empty = std::env::var("REVER_PREVIEW_EMPTY").is_ok();
        if !empty {
            fixtures(&mut store);
        }
        let store = Arc::new(Mutex::new(store));
        let (lang, theme_mode) = (Lang::detect(), ThemeMode::from_env());
        let console = cx.new(|cx| {
            let mut panel = ConsolePanel::new(store.clone(), lang, theme_mode, cx);
            if !empty {
                panel.set_input("const rows = await fetch('/api/usage').then((r) => r.json())\nrows.filter((row) => row.cost > 0)", cx);
            }
            panel
        });
        let exceptions = cx.new(|cx| {
            let mut panel = ExceptionsPanel::new(store, lang, theme_mode, cx);
            panel.toggle(0, cx);
            panel
        });
        cx.new(|_| Preview {
            console,
            exceptions,
            theme_mode,
        })
    });
}
