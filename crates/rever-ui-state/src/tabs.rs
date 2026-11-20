//! Port of `src/renderer/src/stores/tabs.ts` — the open tabs, the active one,
//! and the session restore that reopens them after a restart.

use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::storage::Storage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyScheme {
    Http,
    Https,
    Socks5,
}

/// Per-tab upstream proxy. Mirrors main/tab-proxy.ts TabProxyConfig. Held in
/// memory only (tabs aren't persisted), so it resets on restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
    pub enabled: bool,
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tab {
    pub id: String,
    pub url: String,
    pub title: String,
    pub web_contents_id: Option<u64>,
    /// Set when the tab is first created. Used as the webview's `src` exactly
    /// once — afterwards the user/AI drives navigation on the view itself, so
    /// the view is not re-mounted.
    pub initial_url: String,
    /// `None` = direct connection (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
    /// Isolated session partition (proxy or non-default profile tabs). `None` =
    /// the shared persist:rever-shared partition. Set once at creation — a
    /// webview's partition cannot change after mount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<String>,
    /// Which named profile this tab browses under. `None` / 'default' = the
    /// Default (shared) profile. Used for the tab-bar label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
}

/// The optional second argument of `add_tab`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddTabOptions {
    /// Whether the new tab becomes the active one.
    pub activate: bool,
    pub proxy: Option<ProxyConfig>,
    pub partition: Option<String>,
    pub profile_id: Option<String>,
}

impl Default for AddTabOptions {
    fn default() -> Self {
        Self {
            activate: true,
            proxy: None,
            partition: None,
            profile_id: None,
        }
    }
}

/// The fields `update_tab` may change (`id` and `initial_url` never do). The
/// outer `None` leaves a field alone; for the optional fields `Some(None)`
/// clears it, as spreading an explicit `undefined` did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabPatch {
    pub url: Option<String>,
    pub title: Option<String>,
    pub web_contents_id: Option<Option<u64>>,
    pub proxy: Option<Option<ProxyConfig>>,
    pub partition: Option<Option<String>>,
    pub profile_id: Option<Option<String>>,
}

const INITIAL_URL: &str = "https://github.com/greekr4/rever-browser";

// Session restore: remember the open tabs across restarts so the app reopens
// where the user left off instead of always jumping to the home page. Proxy
// tabs are left out — their proxy config lives only in memory, so restoring the
// URL without it would load the page directly (wrong). Isolated-session
// partitions for named profiles are stable across restarts and kept.
pub const STORE_KEY: &str = "rev:tabs";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedTab {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SavedTabs {
    pub tabs: Vec<SavedTab>,
    /// Any JSON number is accepted, so this is not yet a valid index.
    pub active_index: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SavedTabsOut {
    tabs: Vec<SavedTab>,
    active_index: usize,
}

pub fn serialize_tabs(tabs: &[Tab], active_id: &str) -> String {
    let savable: Vec<&Tab> = tabs
        .iter()
        .filter(|t| t.proxy.is_none() && !t.url.is_empty())
        .collect();
    let active_index = savable.iter().position(|t| t.id == active_id).unwrap_or(0);
    let out = SavedTabsOut {
        tabs: savable
            .iter()
            .map(|t| SavedTab {
                url: t.url.clone(),
                title: Some(t.title.clone()),
                profile_id: t.profile_id.clone(),
                partition: t.partition.clone(),
            })
            .collect(),
        active_index,
    };
    // Plain structs of strings and integers always serialize.
    serde_json::to_string(&out).unwrap_or_default()
}

pub fn parse_saved_tabs(raw: Option<&str>) -> Option<SavedTabs> {
    let raw = raw.filter(|r| !r.is_empty())?;
    let saved: Value = serde_json::from_str(raw).ok()?;
    let items = saved.get("tabs")?.as_array()?;
    if items.is_empty() {
        return None;
    }
    let text = |item: &Value, key: &str| item.get(key).and_then(Value::as_str).map(str::to_owned);
    // One entry without a string url rejects the whole save.
    let tabs = items
        .iter()
        .map(|item| {
            Some(SavedTab {
                url: text(item, "url")?,
                title: text(item, "title"),
                profile_id: text(item, "profileId"),
                partition: text(item, "partition"),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let active_index = saved
        .get("activeIndex")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    Some(SavedTabs { tabs, active_index })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabsStore {
    tabs: Vec<Tab>,
    active_id: String,
    next_id: u64,
    /// URLs of recently closed tabs, newest last — backs Cmd/Ctrl+Shift+T.
    closed_urls: Vec<String>,
}

impl Default for TabsStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TabsStore {
    /// One tab on the home page: the state without a saved session.
    pub fn new() -> Self {
        let mut store = Self::empty();
        let id = store.new_id();
        store.tabs.push(Tab {
            id: id.clone(),
            url: INITIAL_URL.to_owned(),
            title: "New Tab".to_owned(),
            web_contents_id: None,
            initial_url: INITIAL_URL.to_owned(),
            proxy: None,
            partition: None,
            profile_id: None,
        });
        store.active_id = id;
        store
    }

    /// Reopens the tabs saved by the previous run, or starts like `new`.
    pub fn load(storage: &dyn Storage) -> Self {
        let Some(saved) = parse_saved_tabs(storage.get(STORE_KEY).as_deref()) else {
            return Self::new();
        };
        let mut store = Self::empty();
        for t in saved.tabs {
            let id = store.new_id();
            store.tabs.push(Tab {
                id,
                title: t
                    .title
                    .filter(|title| !title.is_empty())
                    .unwrap_or_else(|| "New Tab".to_owned()),
                web_contents_id: None,
                initial_url: t.url.clone(),
                url: t.url,
                proxy: None,
                profile_id: t.profile_id,
                partition: t.partition,
            });
        }
        // The saved index is untrusted: clamp it into range. The float-to-int
        // cast saturates and drops a fraction, so it cannot land out of range.
        let last = store.tabs.len().saturating_sub(1);
        let index = (saved.active_index.max(0.0) as usize).min(last);
        match store.tabs.get(index) {
            Some(active) => store.active_id = active.id.clone(),
            // parse_saved_tabs never returns an empty list; stay usable anyway.
            None => return Self::new(),
        }
        store
    }

    fn empty() -> Self {
        Self {
            tabs: Vec::new(),
            active_id: String::new(),
            next_id: 1,
            closed_urls: Vec::new(),
        }
    }

    fn new_id(&mut self) -> String {
        let id = format!("t{}", self.next_id);
        self.next_id += 1;
        id
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active_id(&self) -> &str {
        &self.active_id
    }

    /// Persist the open tabs so a restart reopens them. Call it after every
    /// change, as the store's subscriber did; that subscriber ignored a failed
    /// write, so ignoring the result here matches it.
    pub fn save(&self, storage: &dyn Storage) -> io::Result<()> {
        storage.set(STORE_KEY, &serialize_tabs(&self.tabs, &self.active_id))
    }

    pub fn add_tab(&mut self, url: &str, opts: AddTabOptions) -> String {
        let id = self.new_id();
        self.tabs.push(Tab {
            id: id.clone(),
            url: url.to_owned(),
            title: "New Tab".to_owned(),
            web_contents_id: None,
            initial_url: url.to_owned(),
            proxy: opts.proxy,
            partition: opts.partition,
            profile_id: opts.profile_id,
        });
        if opts.activate {
            self.active_id = id.clone();
        }
        id
    }

    pub fn close_tab(&mut self, id: &str) {
        if self.tabs.len() == 1 {
            return; // never close the last tab
        }
        let Some(idx) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        let closed = self.tabs.remove(idx);
        self.closed_urls.push(closed.url);
        if self.active_id == id {
            // The tab that slid into the closed slot, else the one before it.
            let neighbour = self
                .tabs
                .get(idx)
                .or_else(|| idx.checked_sub(1).and_then(|i| self.tabs.get(i)))
                .or_else(|| self.tabs.first());
            if let Some(neighbour) = neighbour {
                self.active_id = neighbour.id.clone();
            }
        }
    }

    pub fn reopen_tab(&mut self) {
        // A tab closed with an empty URL is popped and dropped, not reopened.
        if let Some(url) = self.closed_urls.pop().filter(|url| !url.is_empty()) {
            self.add_tab(&url, AddTabOptions::default());
        }
    }

    pub fn select_tab(&mut self, id: &str) {
        if self.tabs.iter().any(|t| t.id == id) {
            self.active_id = id.to_owned();
        }
    }

    pub fn update_tab(&mut self, id: &str, patch: TabPatch) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else {
            return;
        };
        if let Some(url) = patch.url {
            tab.url = url;
        }
        if let Some(title) = patch.title {
            tab.title = title;
        }
        if let Some(web_contents_id) = patch.web_contents_id {
            tab.web_contents_id = web_contents_id;
        }
        if let Some(proxy) = patch.proxy {
            tab.proxy = proxy;
        }
        if let Some(partition) = patch.partition {
            tab.partition = partition;
        }
        if let Some(profile_id) = patch.profile_id {
            tab.profile_id = profile_id;
        }
    }

    pub fn set_tab_proxy(&mut self, id: &str, proxy: Option<ProxyConfig>) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
            tab.proxy = proxy;
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::storage::MemoryStorage;

    fn tab(id: &str, url: &str) -> Tab {
        Tab {
            id: id.to_owned(),
            url: url.to_owned(),
            title: "T".to_owned(),
            web_contents_id: None,
            initial_url: url.to_owned(),
            proxy: None,
            partition: None,
            profile_id: None,
        }
    }

    fn proxy() -> ProxyConfig {
        ProxyConfig {
            enabled: true,
            scheme: ProxyScheme::Http,
            host: "h".to_owned(),
            port: 1,
            username: None,
            password: None,
        }
    }

    fn parsed(raw: &str) -> Value {
        serde_json::from_str(raw).unwrap()
    }

    fn ids(store: &TabsStore) -> Vec<&str> {
        store.tabs().iter().map(|t| t.id.as_str()).collect()
    }

    // --- tabs.test.ts: serializeTabs ---

    #[test]
    fn url_title_profileid_partition을_저장하고_활성_인덱스를_담는다() {
        let tabs = [
            tab("t1", "https://a.test"),
            Tab {
                profile_id: Some("work".to_owned()),
                ..tab("t2", "https://b.test")
            },
        ];
        let parsed = parsed(&serialize_tabs(&tabs, "t2"));
        assert_eq!(parsed["tabs"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["tabs"][1]["url"], "https://b.test");
        assert_eq!(parsed["tabs"][1]["profileId"], "work");
        assert_eq!(parsed["activeIndex"], 1);
    }

    #[test]
    fn 프록시_탭과_빈_url_탭은_저장에서_제외한다() {
        let tabs = [
            tab("t1", "https://a.test"),
            Tab {
                proxy: Some(proxy()),
                ..tab("t2", "https://p.test")
            },
            tab("t3", ""),
        ];
        let parsed = parsed(&serialize_tabs(&tabs, "t1"));
        let urls: Vec<&Value> = parsed["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| &t["url"])
            .collect();
        assert_eq!(urls, [&json!("https://a.test")]);
    }

    #[test]
    fn 활성_탭이_저장_제외_대상이면_인덱스_0으로_떨어진다() {
        let tabs = [
            tab("t1", "https://a.test"),
            Tab {
                proxy: Some(proxy()),
                ..tab("t2", "https://p.test")
            },
        ];
        assert_eq!(parsed(&serialize_tabs(&tabs, "t2"))["activeIndex"], 0);
    }

    // --- tabs.test.ts: parseSavedTabs ---

    #[test]
    fn 유효한_저장본을_복원한다() {
        let raw = serialize_tabs(&[tab("t1", "https://a.test")], "t1");
        assert_eq!(
            parse_saved_tabs(Some(&raw)),
            Some(SavedTabs {
                tabs: vec![SavedTab {
                    url: "https://a.test".to_owned(),
                    title: Some("T".to_owned()),
                    profile_id: None,
                    partition: None,
                }],
                active_index: 0.0,
            })
        );
    }

    #[test]
    fn null_깨진_json_빈_탭_url_없는_항목이면_none을_돌려준다() {
        assert_eq!(parse_saved_tabs(None), None);
        assert_eq!(parse_saved_tabs(Some("not json")), None);
        assert_eq!(
            parse_saved_tabs(Some(&json!({ "tabs": [], "activeIndex": 0 }).to_string())),
            None
        );
        assert_eq!(
            parse_saved_tabs(Some(
                &json!({ "tabs": [{ "title": "x" }], "activeIndex": 0 }).to_string()
            )),
            None
        );
    }

    // --- not covered by tabs.test.ts ---

    #[test]
    fn 저장_문자열은_자바스크립트와_같은_키_순서이고_없는_필드는_빠진다() {
        let tabs = [
            tab("t1", "https://a.test"),
            Tab {
                profile_id: Some("work".to_owned()),
                partition: Some("persist:work".to_owned()),
                ..tab("t2", "https://b.test")
            },
        ];
        assert_eq!(
            serialize_tabs(&tabs, "t2"),
            r#"{"tabs":[{"url":"https://a.test","title":"T"},{"url":"https://b.test","title":"T","profileId":"work","partition":"persist:work"}],"activeIndex":1}"#
        );
    }

    #[test]
    fn 저장본_파싱은_빈_문자열과_객체가_아닌_값과_null_항목을_거부한다() {
        for raw in [
            "",
            "null",
            "5",
            "[]",
            r#"{"tabs":{"url":"https://a.test"}}"#,
            r#"{"tabs":[null]}"#,
            r#"{"tabs":[{"url":"https://a.test"},{"url":5}]}"#,
        ] {
            assert_eq!(parse_saved_tabs(Some(raw)), None, "{raw}");
        }
    }

    #[test]
    fn 저장본의_activeindex가_숫자가_아니면_0이다() {
        let saved = parse_saved_tabs(Some(
            r#"{"tabs":[{"url":"https://a.test"}],"activeIndex":"1"}"#,
        ))
        .unwrap();
        assert_eq!(saved.active_index, 0.0);
        assert_eq!(saved.tabs[0].title, None);
        let missing = parse_saved_tabs(Some(r#"{"tabs":[{"url":"https://a.test"}]}"#)).unwrap();
        assert_eq!(missing.active_index, 0.0);
    }

    #[test]
    fn 저장본이_없으면_홈_페이지_탭_하나로_시작한다() {
        let store = TabsStore::load(&MemoryStorage::new());
        assert_eq!(store, TabsStore::new());
        assert_eq!(store.active_id(), "t1");
        assert_eq!(
            store.tabs(),
            [Tab {
                title: "New Tab".to_owned(),
                ..tab("t1", "https://github.com/greekr4/rever-browser")
            }]
        );
    }

    #[test]
    fn 저장한_탭을_다시_불러오면_url과_프로필과_활성_탭을_복원한다() {
        let storage = MemoryStorage::new();
        let mut store = TabsStore::new();
        store.update_tab(
            "t1",
            TabPatch {
                url: Some("https://a.test".to_owned()),
                title: Some("A".to_owned()),
                web_contents_id: Some(Some(7)),
                ..TabPatch::default()
            },
        );
        store.add_tab(
            "https://b.test",
            AddTabOptions {
                partition: Some("persist:work".to_owned()),
                profile_id: Some("work".to_owned()),
                ..AddTabOptions::default()
            },
        );
        store.add_tab(
            "https://p.test",
            AddTabOptions {
                activate: false,
                proxy: Some(proxy()),
                ..AddTabOptions::default()
            },
        );
        store.save(&storage).unwrap();
        assert!(storage.get("rev:tabs").is_some());

        let restored = TabsStore::load(&storage);
        assert_eq!(
            restored.tabs(),
            [
                Tab {
                    title: "A".to_owned(),
                    ..tab("t1", "https://a.test")
                },
                Tab {
                    title: "New Tab".to_owned(),
                    partition: Some("persist:work".to_owned()),
                    profile_id: Some("work".to_owned()),
                    ..tab("t2", "https://b.test")
                },
            ]
        );
        // The proxy tab is not restored, and the view ids start over.
        assert_eq!(restored.active_id(), "t2");
    }

    #[test]
    fn 불러올_때_빈_제목은_new_tab이_되고_활성_인덱스는_범위_안으로_맞춘다() {
        let storage = MemoryStorage::new();
        let load = |active_index: &str| {
            storage
                .set(
                    "rev:tabs",
                    &format!(
                        r#"{{"tabs":[{{"url":"https://a.test","title":""}},{{"url":"https://b.test","title":"B"}}],"activeIndex":{active_index}}}"#
                    ),
                )
                .unwrap();
            TabsStore::load(&storage)
        };
        let store = load("1");
        assert_eq!(store.tabs()[0].title, "New Tab");
        assert_eq!(store.tabs()[1].title, "B");
        assert_eq!(store.active_id(), "t2");
        assert_eq!(load("-3").active_id(), "t1");
        assert_eq!(load("99").active_id(), "t2");
        assert_eq!(load("1e300").active_id(), "t2");
        assert_eq!(load("0.5").active_id(), "t1");
        // New ids continue after the restored ones.
        let mut store = load("0");
        assert_eq!(
            store.add_tab("https://c.test", AddTabOptions::default()),
            "t3"
        );
    }

    #[test]
    fn add_tab은_새_탭을_끝에_붙이고_기본으로_활성화한다() {
        let mut store = TabsStore::new();
        let id = store.add_tab("https://a.test", AddTabOptions::default());
        assert_eq!(id, "t2");
        assert_eq!(store.active_id(), "t2");
        assert_eq!(
            store.tabs()[1],
            Tab {
                title: "New Tab".to_owned(),
                ..tab("t2", "https://a.test")
            }
        );

        let background = store.add_tab(
            "https://b.test",
            AddTabOptions {
                activate: false,
                ..AddTabOptions::default()
            },
        );
        assert_eq!(background, "t3");
        assert_eq!(store.active_id(), "t2");
        assert_eq!(ids(&store), ["t1", "t2", "t3"]);
    }

    #[test]
    fn 마지막_탭은_닫지_않는다() {
        let mut store = TabsStore::new();
        store.close_tab("t1");
        assert_eq!(ids(&store), ["t1"]);
        assert_eq!(store.active_id(), "t1");
        // Nothing was closed, so there is nothing to reopen.
        store.reopen_tab();
        assert_eq!(ids(&store), ["t1"]);
    }

    #[test]
    fn 활성_탭을_닫으면_그_자리로_온_탭을_없으면_앞_탭을_활성화한다() {
        let mut store = TabsStore::new();
        store.add_tab("https://b.test", AddTabOptions::default());
        store.add_tab("https://c.test", AddTabOptions::default());

        store.select_tab("t2");
        store.close_tab("t2");
        assert_eq!(ids(&store), ["t1", "t3"]);
        assert_eq!(store.active_id(), "t3");

        store.close_tab("t3");
        assert_eq!(ids(&store), ["t1"]);
        assert_eq!(store.active_id(), "t1");
    }

    #[test]
    fn 활성이_아닌_탭이나_없는_탭을_닫아도_활성_탭은_그대로다() {
        let mut store = TabsStore::new();
        store.add_tab("https://b.test", AddTabOptions::default());
        store.add_tab("https://c.test", AddTabOptions::default());
        store.close_tab("nope");
        assert_eq!(ids(&store), ["t1", "t2", "t3"]);
        store.close_tab("t1");
        assert_eq!(ids(&store), ["t2", "t3"]);
        assert_eq!(store.active_id(), "t3");
    }

    #[test]
    fn reopen_tab은_가장_최근에_닫은_url부터_새_탭으로_연다() {
        let mut store = TabsStore::new();
        store.add_tab("https://b.test", AddTabOptions::default());
        store.add_tab("https://c.test", AddTabOptions::default());
        store.close_tab("t2");
        store.close_tab("t3");

        store.reopen_tab();
        assert_eq!(store.tabs().last().unwrap().url, "https://c.test");
        assert_eq!(store.active_id(), "t4");
        store.reopen_tab();
        assert_eq!(store.tabs().last().unwrap().url, "https://b.test");
        store.reopen_tab();
        assert_eq!(ids(&store), ["t1", "t4", "t5"]);
    }

    #[test]
    fn 빈_url로_닫힌_탭은_다시_열지_않고_목록에서만_빠진다() {
        let mut store = TabsStore::new();
        store.add_tab("https://b.test", AddTabOptions::default());
        store.add_tab("", AddTabOptions::default());
        store.close_tab("t2");
        store.close_tab("t3");
        store.reopen_tab();
        assert_eq!(ids(&store), ["t1"]);
        store.reopen_tab();
        assert_eq!(store.tabs().last().unwrap().url, "https://b.test");
    }

    #[test]
    fn select_tab은_없는_id를_무시한다() {
        let mut store = TabsStore::new();
        store.add_tab("https://b.test", AddTabOptions::default());
        store.select_tab("t1");
        assert_eq!(store.active_id(), "t1");
        store.select_tab("t9");
        assert_eq!(store.active_id(), "t1");
    }

    #[test]
    fn update_tab은_준_필드만_바꾸고_id와_initial_url은_그대로_둔다() {
        let mut store = TabsStore::new();
        store.set_tab_proxy("t1", Some(proxy()));
        store.update_tab(
            "t1",
            TabPatch {
                url: Some("https://a.test".to_owned()),
                web_contents_id: Some(Some(3)),
                ..TabPatch::default()
            },
        );
        let t = &store.tabs()[0];
        assert_eq!(t.url, "https://a.test");
        assert_eq!(t.title, "New Tab");
        assert_eq!(t.web_contents_id, Some(3));
        assert_eq!(t.initial_url, "https://github.com/greekr4/rever-browser");
        assert_eq!(t.proxy, Some(proxy()));

        // An explicit clear is different from leaving the field out.
        store.update_tab(
            "t1",
            TabPatch {
                web_contents_id: Some(None),
                proxy: Some(None),
                ..TabPatch::default()
            },
        );
        assert_eq!(store.tabs()[0].web_contents_id, None);
        assert_eq!(store.tabs()[0].proxy, None);

        let before = store.clone();
        store.update_tab(
            "t9",
            TabPatch {
                title: Some("x".to_owned()),
                ..TabPatch::default()
            },
        );
        store.set_tab_proxy("t9", Some(proxy()));
        assert_eq!(store, before);
    }

    #[test]
    fn 탭과_프록시는_camelcase_json으로_직렬화된다() {
        let t = Tab {
            web_contents_id: Some(4),
            proxy: Some(ProxyConfig {
                scheme: ProxyScheme::Socks5,
                username: Some("u".to_owned()),
                ..proxy()
            }),
            ..tab("t1", "https://a.test")
        };
        assert_eq!(
            json!(t),
            json!({
                "id": "t1",
                "url": "https://a.test",
                "title": "T",
                "webContentsId": 4,
                "initialUrl": "https://a.test",
                "proxy": { "enabled": true, "scheme": "socks5", "host": "h", "port": 1, "username": "u" }
            })
        );
        assert_eq!(serde_json::from_value::<Tab>(json!(t)).unwrap(), t);
    }
}
