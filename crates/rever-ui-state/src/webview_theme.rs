//! Port of `src/renderer/src/stores/webview-theme.ts` — the color scheme each
//! site is shown in, chosen per origin.

use std::collections::BTreeMap;
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::app_theme::{ResolvedTheme, ThemeMode, resolve_theme};
use crate::storage::{Storage, load_persisted, save_persisted};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebviewTheme {
    #[default]
    Auto,
    Light,
    Dark,
}

const NAME: &str = "rev:webview-theme";

/// The origin of an http(s) URL, `None` for anything else or unparseable.
pub fn origin_from_url(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    // A prefix test, as in the TypeScript: a non-special scheme that merely
    // starts with "http" gets through and has the opaque origin "null".
    if !u.scheme().starts_with("http") {
        return None;
    }
    Some(u.origin().ascii_serialization())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebviewThemeStore {
    by_origin: BTreeMap<String, WebviewTheme>,
}

impl WebviewThemeStore {
    pub fn load(storage: &dyn Storage) -> Self {
        let state = load_persisted(storage, NAME);
        let by_origin = match state.get("byOrigin") {
            // An origin with an unreadable theme is left out, which reads as auto.
            Some(Value::Object(entries)) => entries
                .iter()
                .filter_map(|(origin, theme)| {
                    Some((origin.clone(), WebviewTheme::deserialize(theme).ok()?))
                })
                .collect(),
            _ => BTreeMap::new(),
        };
        Self { by_origin }
    }

    pub fn by_origin(&self) -> &BTreeMap<String, WebviewTheme> {
        &self.by_origin
    }

    pub fn get(&self, origin: &str) -> WebviewTheme {
        self.by_origin.get(origin).copied().unwrap_or_default()
    }

    /// The state changes even when the write fails, as with `persist`.
    pub fn set(
        &mut self,
        storage: &dyn Storage,
        origin: &str,
        theme: WebviewTheme,
    ) -> io::Result<()> {
        // Auto is the absence of an entry, so the map only holds overrides.
        if theme == WebviewTheme::Auto {
            self.by_origin.remove(origin);
        } else {
            self.by_origin.insert(origin.to_owned(), theme);
        }
        save_persisted(storage, NAME, self)
    }

    /// Cycle Auto → Light → Dark → Auto and return the new theme, which is in
    /// effect even when the write fails.
    pub fn cycle(&mut self, storage: &dyn Storage, origin: &str) -> (WebviewTheme, io::Result<()>) {
        let next = match self.get(origin) {
            WebviewTheme::Auto => WebviewTheme::Light,
            WebviewTheme::Light => WebviewTheme::Dark,
            WebviewTheme::Dark => WebviewTheme::Auto,
        };
        (next, self.set(storage, origin, next))
    }
}

/// The `prefers-color-scheme` to emulate inside the webview. 'auto' follows the
/// app's own theme (not the OS) so an app in light mode doesn't show a dark site.
pub fn resolve_webview_theme(
    theme: WebviewTheme,
    app_mode: ThemeMode,
    system_prefers_dark: bool,
) -> ResolvedTheme {
    match theme {
        WebviewTheme::Auto => resolve_theme(app_mode, system_prefers_dark),
        WebviewTheme::Light => ResolvedTheme::Light,
        WebviewTheme::Dark => ResolvedTheme::Dark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn origin_from_url은_http와_https의_origin만_돌려준다() {
        assert_eq!(
            origin_from_url("https://example.com/a/b?c=1#d").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            origin_from_url("http://localhost:8777/x").as_deref(),
            Some("http://localhost:8777")
        );
        // Default ports are dropped and the host is lowercased, as in `URL.origin`.
        assert_eq!(
            origin_from_url("HTTPS://Example.COM:443/").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            origin_from_url("http://user:pw@example.com:80/").as_deref(),
            Some("http://example.com")
        );
        assert_eq!(
            origin_from_url("https://한글.kr/").as_deref(),
            Some("https://xn--bj0bj06e.kr")
        );
        // The forgiving forms the URL parser accepts; expected values are
        // what `new URL(x).origin` gives.
        for (url, origin) in [
            ("http:foo", "http://foo"),
            ("https:/bar/baz", "https://bar"),
            ("http://[::1]:3000/x", "http://[::1]:3000"),
            ("http://1.2.3.4./", "http://1.2.3.4"),
            ("http://0x7f.1/", "http://127.0.0.1"),
        ] {
            assert_eq!(origin_from_url(url).as_deref(), Some(origin), "{url}");
        }
    }

    #[test]
    fn origin_from_url은_다른_스킴과_잘못된_url에_none을_돌려준다() {
        for url in [
            "about:blank",
            "file:///tmp/a.html",
            "chrome://settings",
            "data:text/html,hi",
            "example.com",
            "",
            "https://",
        ] {
            assert_eq!(origin_from_url(url), None, "{url}");
        }
    }

    #[test]
    fn http로_시작하는_비표준_스킴은_불투명_origin_null이_된다() {
        // new URL('httpx://foo').origin === 'null'
        assert_eq!(origin_from_url("httpx://foo").as_deref(), Some("null"));
    }

    #[test]
    fn 설정이_없는_origin은_auto다() {
        let store = WebviewThemeStore::load(&MemoryStorage::new());
        assert_eq!(store.get("https://a.test"), WebviewTheme::Auto);
    }

    #[test]
    fn set은_origin별로_저장하고_auto는_항목을_지운다() {
        let storage = MemoryStorage::new();
        let mut store = WebviewThemeStore::load(&storage);
        store
            .set(&storage, "https://a.test", WebviewTheme::Dark)
            .unwrap();
        store
            .set(&storage, "https://b.test", WebviewTheme::Light)
            .unwrap();
        assert_eq!(store.get("https://a.test"), WebviewTheme::Dark);
        assert_eq!(
            storage.get("rev:webview-theme").as_deref(),
            Some(
                r#"{"state":{"byOrigin":{"https://a.test":"dark","https://b.test":"light"}},"version":0}"#
            )
        );
        assert_eq!(WebviewThemeStore::load(&storage), store);

        store
            .set(&storage, "https://a.test", WebviewTheme::Auto)
            .unwrap();
        assert!(!store.by_origin().contains_key("https://a.test"));
        assert_eq!(
            storage.get("rev:webview-theme").as_deref(),
            Some(r#"{"state":{"byOrigin":{"https://b.test":"light"}},"version":0}"#)
        );
    }

    #[test]
    fn cycle은_auto_light_dark_순서로_돌고_새_테마를_돌려준다() {
        let storage = MemoryStorage::new();
        let mut store = WebviewThemeStore::load(&storage);
        let origin = "https://a.test";
        assert_eq!(store.cycle(&storage, origin).0, WebviewTheme::Light);
        assert_eq!(store.cycle(&storage, origin).0, WebviewTheme::Dark);
        assert_eq!(store.get(origin), WebviewTheme::Dark);
        let (next, written) = store.cycle(&storage, origin);
        assert_eq!(next, WebviewTheme::Auto);
        assert!(written.is_ok());
        assert!(store.by_origin().is_empty());
        // Other origins are untouched.
        assert_eq!(store.get("https://b.test"), WebviewTheme::Auto);
    }

    #[test]
    fn 읽을_수_없는_저장_항목은_건너뛴다() {
        let storage = MemoryStorage::new();
        storage
            .set(
                "rev:webview-theme",
                r#"{"state":{"byOrigin":{"https://a.test":"dark","https://b.test":"sepia","https://c.test":5}},"version":0}"#,
            )
            .unwrap();
        let store = WebviewThemeStore::load(&storage);
        assert_eq!(store.get("https://a.test"), WebviewTheme::Dark);
        assert_eq!(store.get("https://b.test"), WebviewTheme::Auto);
        assert_eq!(store.by_origin().len(), 1);

        storage
            .set(
                "rev:webview-theme",
                r#"{"state":{"byOrigin":["dark"]},"version":0}"#,
            )
            .unwrap();
        assert!(WebviewThemeStore::load(&storage).by_origin().is_empty());
    }

    #[test]
    fn auto는_os가_아니라_앱_테마를_따른다() {
        // App in light mode on a dark OS: the site stays light.
        assert_eq!(
            resolve_webview_theme(WebviewTheme::Auto, ThemeMode::Light, true),
            ResolvedTheme::Light
        );
        assert_eq!(
            resolve_webview_theme(WebviewTheme::Auto, ThemeMode::Dark, false),
            ResolvedTheme::Dark
        );
        assert_eq!(
            resolve_webview_theme(WebviewTheme::Auto, ThemeMode::System, true),
            ResolvedTheme::Dark
        );
        assert_eq!(
            resolve_webview_theme(WebviewTheme::Light, ThemeMode::Dark, true),
            ResolvedTheme::Light
        );
        assert_eq!(
            resolve_webview_theme(WebviewTheme::Dark, ThemeMode::Light, false),
            ResolvedTheme::Dark
        );
    }
}
