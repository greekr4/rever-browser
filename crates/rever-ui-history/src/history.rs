//! Port of `src/renderer/src/stores/history.ts`: the pages each tab has visited.
//!
//! The zustand store becomes [`HistoryStore`]; its `persist` middleware becomes a write
//! to the [`Storage`] it was built with, under the same key and in the same JSON shape.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::storage::{Storage, decode_list, load_state, save_state};

/// The `name` of the persisted store.
pub const STORAGE_KEY: &str = "rev:history";
pub const MAX_ENTRIES: usize = 1000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HistoryEntry {
    pub id: String,
    pub tab_id: String,
    pub url: String,
    pub title: String,
    /// Milliseconds since the Unix epoch (`Date.now()`).
    pub visited_at: u64,
}

/// What a caller knows about a visit: `Omit<HistoryEntry, 'id' | 'visitedAt'>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewHistoryEntry {
    pub tab_id: String,
    pub url: String,
    pub title: String,
}

pub struct HistoryStore {
    entries: Vec<HistoryEntry>,
    storage: Box<dyn Storage>,
}

impl HistoryStore {
    /// Starts from what `storage` holds, as zustand's hydration does. A list longer than
    /// [`MAX_ENTRIES`] is kept whole until the next [`push`](Self::push) trims it.
    pub fn new(storage: Box<dyn Storage>) -> Self {
        let entries = decode_list(load_state(storage.as_ref(), STORAGE_KEY), "entries");
        Self { entries, storage }
    }

    /// Oldest first.
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    pub fn push(&mut self, entry: NewHistoryEntry) {
        self.push_at(entry, now_ms());
    }

    /// [`push`](Self::push) with the clock passed in.
    pub fn push_at(&mut self, entry: NewHistoryEntry, now_ms: u64) {
        let repeat = self
            .entries
            .last()
            .is_some_and(|last| last.tab_id == entry.tab_id && last.url == entry.url);
        if !repeat {
            self.entries.push(HistoryEntry {
                id: new_id('h', now_ms),
                tab_id: entry.tab_id,
                url: entry.url,
                title: entry.title,
                visited_at: now_ms,
            });
            let excess = self.entries.len().saturating_sub(MAX_ENTRIES);
            self.entries.drain(..excess);
        }
        self.persist();
    }

    /// Sets the title of the most recent visit of `url` in `tab_id`.
    pub fn update_title(&mut self, tab_id: &str, url: &str, title: &str) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .rev()
            .find(|entry| entry.tab_id == tab_id && entry.url == url)
            && entry.title != title
        {
            entry.title = title.to_string();
        }
        self.persist();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.persist();
    }

    // zustand's persist writes after every `set`, including one that changes nothing,
    // so every action ends here.
    fn persist(&mut self) {
        save_state(
            self.storage.as_mut(),
            STORAGE_KEY,
            json!({ "entries": self.entries }),
        );
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// `Number#toString(36)` for a whole number.
fn base36(mut number: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut digits = Vec::new();
    loop {
        digits.push(DIGITS[(number % 36) as usize]);
        number /= 36;
        if number == 0 {
            break;
        }
    }
    digits.reverse();
    String::from_utf8(digits).unwrap_or_default()
}

/// `newId()`: the prefix, the time in base 36, then six random base-36 digits.
pub(crate) fn new_id(prefix: char, now_ms: u64) -> String {
    // A fresh RandomState is keyed from the OS: std's randomness without a `rand` dependency.
    let random = RandomState::new().build_hasher().finish() % 36u64.pow(6);
    format!("{prefix}{}{:0>6}", base36(now_ms), base36(random))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{MemoryStorage, SharedStorage};
    use std::collections::HashSet;

    fn visit(tab_id: &str, url: &str, title: &str) -> NewHistoryEntry {
        NewHistoryEntry {
            tab_id: tab_id.into(),
            url: url.into(),
            title: title.into(),
        }
    }

    fn empty_store() -> HistoryStore {
        HistoryStore::new(Box::new(MemoryStorage::default()))
    }

    fn store_from(json: &str) -> HistoryStore {
        let mut storage = MemoryStorage::default();
        storage.set(STORAGE_KEY, json);
        HistoryStore::new(Box::new(storage))
    }

    #[test]
    fn 처음에는_기록이_없다() {
        assert!(empty_store().entries().is_empty());
    }

    #[test]
    fn 방문을_넣으면_id와_방문_시각을_붙여_끝에_추가한다() {
        let mut store = empty_store();
        store.push_at(visit("t1", "https://a.test/", "A"), 1_700_000_000_000);
        store.push_at(visit("t1", "https://b.test/", "B"), 1_700_000_000_500);
        let entries = store.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].tab_id, "t1");
        assert_eq!(entries[0].url, "https://a.test/");
        assert_eq!(entries[0].title, "A");
        assert_eq!(entries[0].visited_at, 1_700_000_000_000);
        assert_eq!(entries[1].url, "https://b.test/");
        assert_ne!(entries[0].id, entries[1].id);
    }

    #[test]
    fn push는_지금_시각을_방문_시각으로_쓴다() {
        let before = now_ms();
        let mut store = empty_store();
        store.push(visit("t1", "https://a.test/", "A"));
        let visited_at = store.entries()[0].visited_at;
        assert!(before <= visited_at && visited_at <= now_ms());
    }

    #[test]
    fn 마지막_항목과_탭과_url이_같으면_넣지_않는다() {
        let mut store = empty_store();
        store.push_at(visit("t1", "https://a.test/", "A"), 1);
        // 제목이 달라도 탭과 URL이 같으면 같은 방문이다.
        store.push_at(visit("t1", "https://a.test/", "다른 제목"), 2);
        assert_eq!(store.entries().len(), 1);
        assert_eq!(store.entries()[0].title, "A");
        assert_eq!(store.entries()[0].visited_at, 1);
    }

    #[test]
    fn 탭이나_url이_다르면_넣는다() {
        let mut store = empty_store();
        store.push_at(visit("t1", "https://a.test/", "A"), 1);
        store.push_at(visit("t2", "https://a.test/", "A"), 2);
        store.push_at(visit("t2", "https://b.test/", "B"), 3);
        assert_eq!(store.entries().len(), 3);
    }

    #[test]
    fn 같은_방문이라도_마지막_항목이_아니면_다시_넣는다() {
        let mut store = empty_store();
        store.push_at(visit("t1", "https://a.test/", "A"), 1);
        store.push_at(visit("t1", "https://b.test/", "B"), 2);
        store.push_at(visit("t1", "https://a.test/", "A"), 3);
        assert_eq!(store.entries().len(), 3);
    }

    #[test]
    fn 천_개를_넘으면_가장_오래된_것부터_버린다() {
        let mut store = empty_store();
        for index in 0..(MAX_ENTRIES as u64 + 5) {
            store.push_at(visit("t1", &format!("https://a.test/{index}"), ""), index);
        }
        assert_eq!(store.entries().len(), MAX_ENTRIES);
        assert_eq!(store.entries()[0].url, "https://a.test/5");
        assert_eq!(
            store.entries()[MAX_ENTRIES - 1].url,
            format!("https://a.test/{}", MAX_ENTRIES + 4)
        );
    }

    #[test]
    fn 제목_갱신은_탭과_url이_맞는_가장_최근_항목만_바꾼다() {
        let mut store = empty_store();
        store.push_at(visit("t1", "https://a.test/", "old 1"), 1);
        store.push_at(visit("t1", "https://b.test/", "B"), 2);
        store.push_at(visit("t1", "https://a.test/", "old 2"), 3);
        store.push_at(visit("t2", "https://a.test/", "other tab"), 4);
        store.update_title("t1", "https://a.test/", "new");
        let titles: Vec<&str> = store.entries().iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["old 1", "B", "new", "other tab"]);
    }

    #[test]
    fn 제목_갱신은_맞는_항목이_없으면_아무것도_바꾸지_않는다() {
        let mut store = empty_store();
        store.push_at(visit("t1", "https://a.test/", "A"), 1);
        let before = store.entries().to_vec();
        store.update_title("t9", "https://a.test/", "new");
        store.update_title("t1", "https://zzz.test/", "new");
        store.update_title("t1", "https://a.test/", "A");
        assert_eq!(store.entries(), before);
    }

    #[test]
    fn 비우면_기록이_모두_사라진다() {
        let mut store = empty_store();
        store.push_at(visit("t1", "https://a.test/", "A"), 1);
        store.clear();
        assert!(store.entries().is_empty());
    }

    #[test]
    fn 바뀔_때마다_zustand와_같은_키와_모양으로_저장한다() {
        let shared = SharedStorage::default();
        let mut store = HistoryStore::new(Box::new(shared.clone()));
        store.push_at(visit("t1", "https://a.test/", "A"), 1_700_000_000_000);
        let stored: serde_json::Value =
            serde_json::from_str(&shared.get("rev:history").unwrap()).unwrap();
        let id = store.entries()[0].id.clone();
        assert_eq!(
            stored,
            json!({
                "state": { "entries": [{
                    "id": id,
                    "tabId": "t1",
                    "url": "https://a.test/",
                    "title": "A",
                    "visitedAt": 1_700_000_000_000u64
                }] },
                "version": 0
            })
        );

        store.update_title("t1", "https://a.test/", "제목");
        assert!(
            shared
                .get(STORAGE_KEY)
                .unwrap()
                .contains(r#""title":"제목""#)
        );

        store.clear();
        assert_eq!(
            shared.get(STORAGE_KEY).as_deref(),
            Some(r#"{"state":{"entries":[]},"version":0}"#)
        );
    }

    #[test]
    fn 아무것도_바꾸지_않는_동작도_저장은_한다() {
        // zustand persist는 set이 불릴 때마다 쓴다.
        let shared = SharedStorage::default();
        let mut store = HistoryStore::new(Box::new(shared.clone()));
        assert_eq!(shared.get(STORAGE_KEY), None);
        store.update_title("t1", "https://a.test/", "A");
        assert_eq!(
            shared.get(STORAGE_KEY).as_deref(),
            Some(r#"{"state":{"entries":[]},"version":0}"#)
        );
    }

    #[test]
    fn 저장된_기록에서_다시_시작한다() {
        let shared = SharedStorage::default();
        let mut first = HistoryStore::new(Box::new(shared.clone()));
        first.push_at(visit("t1", "https://a.test/", "A"), 10);
        first.push_at(visit("t2", "https://b.test/", "B"), 20);
        let second = HistoryStore::new(Box::new(shared));
        assert_eq!(second.entries(), first.entries());
    }

    #[test]
    fn electron_앱이_쓴_json을_읽는다() {
        let store = store_from(
            r#"{"state":{"entries":[{"tabId":"tab-1","url":"https://a.test/","title":"A","id":"hlx1abc123","visitedAt":1700000000000}]},"version":0}"#,
        );
        assert_eq!(
            store.entries(),
            [HistoryEntry {
                id: "hlx1abc123".into(),
                tab_id: "tab-1".into(),
                url: "https://a.test/".into(),
                title: "A".into(),
                visited_at: 1_700_000_000_000,
            }]
        );
    }

    #[test]
    fn 깨진_저장값은_빈_기록으로_시작한다() {
        assert!(store_from("{oops").entries().is_empty());
        assert!(store_from("null").entries().is_empty());
        assert!(store_from(r#"{"state":{}}"#).entries().is_empty());
        assert!(
            store_from(r#"{"state":{"entries":"nope"},"version":0}"#)
                .entries()
                .is_empty()
        );
    }

    #[test]
    fn 버전이_다른_저장값은_읽지_않는다() {
        let store = store_from(
            r#"{"state":{"entries":[{"id":"h1","tabId":"t","url":"u","title":"","visitedAt":1}]},"version":2}"#,
        );
        assert!(store.entries().is_empty());
    }

    #[test]
    fn 깨진_항목은_건너뛰고_빠진_필드는_비워_둔다() {
        let store = store_from(
            r#"{"state":{"entries":[42,{"id":"h1","url":"https://a.test/"},{"id":"h2","title":null},"x"]},"version":0}"#,
        );
        assert_eq!(
            store.entries(),
            [HistoryEntry {
                id: "h1".into(),
                url: "https://a.test/".into(),
                ..Default::default()
            }]
        );
    }

    #[test]
    fn 천_개가_넘는_저장값은_다음_push에서_잘린다() {
        let entries: Vec<_> = (0..MAX_ENTRIES + 3)
            .map(|index| json!({ "id": format!("h{index}"), "tabId": "t", "url": format!("u{index}"), "title": "", "visitedAt": index }))
            .collect();
        let mut store =
            store_from(&json!({ "state": { "entries": entries }, "version": 0 }).to_string());
        assert_eq!(store.entries().len(), MAX_ENTRIES + 3);
        store.push_at(visit("t", "new", ""), 5000);
        assert_eq!(store.entries().len(), MAX_ENTRIES);
        assert_eq!(store.entries()[0].id, "h4");
    }

    #[test]
    fn 숫자를_36진수로_쓴다() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
        // (1700000000000).toString(36)
        assert_eq!(base36(1_700_000_000_000), "loyw3v28");
        assert_eq!(base36(u64::MAX), "3w5e11264sgsf");
    }

    #[test]
    fn id는_접두사_시각_무작위_여섯_자리다() {
        let id = new_id('h', 1_700_000_000_000);
        assert!(id.starts_with("hloyw3v28"));
        let suffix = &id["hloyw3v28".len()..];
        assert_eq!(suffix.len(), 6);
        assert!(
            suffix
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
        );
    }

    #[test]
    fn 같은_시각에_만든_id도_서로_다르다() {
        let ids: HashSet<String> = (0..200).map(|_| new_id('h', 1)).collect();
        assert_eq!(ids.len(), 200);
    }
}
