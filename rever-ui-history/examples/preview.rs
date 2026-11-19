//! Opens this crate's view alone with fixture data. See rever_ui_kit::preview.
//!
//! The bookmark bar sits where the app has it (under the toolbar, on the toolbar's
//! color) and the history panel fills the rest. The line at the bottom shows the last
//! URL either view asked the host to open.

use gpui::{Context, Entity, SharedString, Window, div, prelude::*, px};
use rever_ui_history::OpenUrl;
use rever_ui_history::bookmark_bar::BookmarkBar;
use rever_ui_history::bookmarks::{self, BookmarksStore};
use rever_ui_history::history::{self, HistoryStore};
use rever_ui_history::history_panel::HistoryPanel;
use rever_ui_history::storage::{MemoryStorage, Storage};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use serde_json::{Value, json};

const MINUTE: u64 = 60_000;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// (age in milliseconds, URL, title)
const VISITS: &[(u64, &str, &str)] = &[
    (
        9 * DAY + 3 * HOUR,
        "https://news.ycombinator.com/",
        "Hacker News",
    ),
    (
        9 * DAY + 2 * HOUR,
        "https://news.ycombinator.com/item?id=41234567",
        "Show HN: A browser for reverse-engineering web APIs | Hacker News",
    ),
    (
        3 * DAY + 5 * HOUR,
        "https://developer.mozilla.org/en-US/docs/Web/API/Fetch_API",
        "Fetch API - Web APIs | MDN",
    ),
    (
        3 * DAY + 4 * HOUR,
        "https://chromedevtools.github.io/devtools-protocol/tot/Network/",
        "Chrome DevTools Protocol - Network domain",
    ),
    (
        DAY + 6 * HOUR,
        "https://docs.rs/gpui/latest/gpui/",
        "gpui - Rust",
    ),
    (
        DAY + 5 * HOUR,
        "https://servo.org/",
        "Servo aims to empower developers with a lightweight, high-performance alternative for embedding web technologies in applications.",
    ),
    (DAY + 2 * HOUR, "about:blank", ""),
    (
        DAY + HOUR,
        "http://127.0.0.1:8777/snapshot-fixture.html",
        "Snapshot fixture",
    ),
    (
        5 * HOUR,
        "https://www.google.com/search?q=gpui+uniform_list+scroll+handle&sourceid=chrome&ie=UTF-8",
        "gpui uniform_list scroll handle - Google Search",
    ),
    (
        4 * HOUR,
        "https://github.com/zed-industries/zed/tree/main/crates/gpui",
        "zed/crates/gpui at main · zed-industries/zed",
    ),
    (
        3 * HOUR,
        "https://api.github.com/repos/zed-industries/zed/issues?state=open&per_page=100",
        "",
    ),
    (2 * HOUR, "https://www.naver.com/", "NAVER"),
    (
        HOUR,
        "https://ko.wikipedia.org/wiki/리버스_엔지니어링",
        "리버스 엔지니어링 - 위키백과, 우리 모두의 백과사전",
    ),
    (
        20 * MINUTE,
        "https://github.com/greekr4/rever-browser/pulls",
        "Pull requests · greekr4/rever-browser",
    ),
    (
        2 * MINUTE,
        "https://github.com/greekr4/rever-browser",
        "greekr4/rever-browser: AI-agent-driven browser for reverse-engineering web APIs",
    ),
];

const BOOKMARKS: &[(&str, &str)] = &[
    ("https://github.com/greekr4/rever-browser", "rever-browser"),
    ("https://news.ycombinator.com/", "Hacker News"),
    (
        "https://developer.mozilla.org/en-US/docs/Web/API",
        "Web APIs | MDN — a title long enough to be cut off",
    ),
    (
        "https://chromedevtools.github.io/devtools-protocol/",
        "DevTools Protocol",
    ),
    ("about:blank", "about:blank"),
    ("https://www.naver.com/", "네이버"),
    ("https://docs.rs/gpui/latest/gpui/", "gpui - Rust"),
];

/// The stores are seeded through storage, in the JSON the Electron app writes, so the
/// preview also shows that hydration reads it.
fn fixture_storage(key: &str, field: &str, items: Vec<Value>) -> Box<dyn Storage> {
    let mut storage = MemoryStorage::default();
    storage.set(
        key,
        &json!({ "state": { field: items }, "version": 0 }).to_string(),
    );
    Box::new(storage)
}

struct Preview {
    bar: Entity<BookmarkBar>,
    panel: Entity<HistoryPanel>,
    theme_mode: ThemeMode,
    last_request: Option<SharedString>,
}

impl Preview {
    fn new(cx: &mut Context<Self>) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        let visits = VISITS
            .iter()
            .enumerate()
            .map(|(index, (age, url, title))| {
                json!({ "id": format!("h{index}"), "tabId": "tab-1", "url": url, "title": title, "visitedAt": now - age })
            })
            .collect();
        let marks = BOOKMARKS
            .iter()
            .enumerate()
            .map(|(index, (url, title))| {
                json!({ "id": format!("b{index}"), "url": url, "title": title, "addedAt": now })
            })
            .collect();
        let history =
            cx.new(|_| HistoryStore::new(fixture_storage(history::STORAGE_KEY, "entries", visits)));
        let bookmarks = cx.new(|_| {
            BookmarksStore::new(fixture_storage(bookmarks::STORAGE_KEY, "bookmarks", marks))
        });

        let (lang, theme_mode) = (Lang::detect(), ThemeMode::from_env());
        let bar = cx.new(|cx| BookmarkBar::new(bookmarks, lang, theme_mode, cx));
        let panel = cx.new(|cx| HistoryPanel::new(history, lang, theme_mode, cx));
        cx.subscribe(&bar, Self::on_open).detach();
        cx.subscribe(&panel, Self::on_open).detach();
        Self {
            bar,
            panel,
            theme_mode,
            last_request: None,
        }
    }

    fn on_open<V>(&mut self, _: Entity<V>, event: &OpenUrl, cx: &mut Context<Self>) {
        self.last_request = Some(event.0.clone().into());
        cx.notify();
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
            .child(
                div()
                    .flex_none()
                    .pt(px(6.0))
                    .bg(theme.chrome_toolbar)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(self.bar.clone()),
            )
            .child(div().flex_1().min_h_0().child(self.panel.clone()))
            .child(
                div()
                    .flex_none()
                    .px(px(10.0))
                    .pb(px(6.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_dim)
                    .child(match &self.last_request {
                        Some(url) => format!("OpenUrl → {url}"),
                        None => "OpenUrl → (click an entry or a bookmark)".to_string(),
                    }),
            )
    }
}

fn main() {
    rever_ui_kit::preview::run(760.0, 560.0, |_, cx| cx.new(Preview::new));
}
