//! Port of `src/renderer/src/stores/bookmarks.ts`: the pages pinned to the bookmark bar.
//!
//! The zustand store becomes [`BookmarksStore`]; its `persist` middleware becomes a write
//! to the [`Storage`] it was built with, under the same key and in the same JSON shape.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::history::{new_id, now_ms};
use crate::storage::{Storage, decode_list, load_state, save_state};

/// The `name` of the persisted store.
pub const STORAGE_KEY: &str = "rev:bookmarks";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Bookmark {
    pub id: String,
    pub url: String,
    pub title: String,
    /// Milliseconds since the Unix epoch (`Date.now()`).
    pub added_at: u64,
}

pub struct BookmarksStore {
    bookmarks: Vec<Bookmark>,
    storage: Box<dyn Storage>,
}

impl BookmarksStore {
    /// Starts from what `storage` holds, as zustand's hydration does.
    pub fn new(storage: Box<dyn Storage>) -> Self {
        let bookmarks = decode_list(load_state(storage.as_ref(), STORAGE_KEY), "bookmarks");
        Self { bookmarks, storage }
    }

    /// In the order they were added.
    pub fn bookmarks(&self) -> &[Bookmark] {
        &self.bookmarks
    }

    /// Adds `url` unless it is bookmarked already. An empty title becomes the URL.
    pub fn add(&mut self, url: &str, title: &str) {
        self.add_at(url, title, now_ms());
    }

    /// [`add`](Self::add) with the clock passed in.
    pub fn add_at(&mut self, url: &str, title: &str, now_ms: u64) {
        if !self.bookmarks.iter().any(|bookmark| bookmark.url == url) {
            self.bookmarks.push(Bookmark {
                id: new_id('b', now_ms),
                url: url.to_string(),
                title: if title.is_empty() { url } else { title }.to_string(),
                added_at: now_ms,
            });
        }
        self.persist();
    }

    pub fn remove(&mut self, id: &str) {
        self.bookmarks.retain(|bookmark| bookmark.id != id);
        self.persist();
    }

    pub fn remove_by_url(&mut self, url: &str) {
        self.bookmarks.retain(|bookmark| bookmark.url != url);
        self.persist();
    }

    // zustand's persist writes after every `set`, including one that changes nothing,
    // so every action ends here.
    fn persist(&mut self) {
        save_state(
            self.storage.as_mut(),
            STORAGE_KEY,
            json!({ "bookmarks": self.bookmarks }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{MemoryStorage, SharedStorage};

    fn empty_store() -> BookmarksStore {
        BookmarksStore::new(Box::new(MemoryStorage::default()))
    }

    fn store_from(json: &str) -> BookmarksStore {
        let mut storage = MemoryStorage::default();
        storage.set(STORAGE_KEY, json);
        BookmarksStore::new(Box::new(storage))
    }

    fn urls(store: &BookmarksStore) -> Vec<&str> {
        store.bookmarks().iter().map(|b| b.url.as_str()).collect()
    }

    #[test]
    fn 처음에는_북마크가_없다() {
        assert!(empty_store().bookmarks().is_empty());
    }

    #[test]
    fn 추가하면_id와_추가_시각을_붙여_끝에_넣는다() {
        let mut store = empty_store();
        store.add_at("https://a.test/", "A", 1_700_000_000_000);
        store.add_at("https://b.test/", "B", 1_700_000_000_001);
        let bookmarks = store.bookmarks();
        assert_eq!(urls(&store), ["https://a.test/", "https://b.test/"]);
        assert_eq!(bookmarks[0].title, "A");
        assert_eq!(bookmarks[0].added_at, 1_700_000_000_000);
        assert!(bookmarks[0].id.starts_with("bloyw3v28"));
        assert_eq!(bookmarks[0].id.len(), "bloyw3v28".len() + 6);
        assert_ne!(bookmarks[0].id, bookmarks[1].id);
    }

    #[test]
    fn add는_지금_시각을_추가_시각으로_쓴다() {
        let before = now_ms();
        let mut store = empty_store();
        store.add("https://a.test/", "A");
        let added_at = store.bookmarks()[0].added_at;
        assert!(before <= added_at && added_at <= now_ms());
    }

    #[test]
    fn 이미_있는_url은_다시_추가하지_않는다() {
        let mut store = empty_store();
        store.add_at("https://a.test/", "A", 1);
        store.add_at("https://b.test/", "B", 2);
        store.add_at("https://a.test/", "다른 제목", 3);
        assert_eq!(urls(&store), ["https://a.test/", "https://b.test/"]);
        assert_eq!(store.bookmarks()[0].title, "A");
        assert_eq!(store.bookmarks()[0].added_at, 1);
    }

    #[test]
    fn 제목이_비어_있으면_url을_제목으로_쓴다() {
        let mut store = empty_store();
        store.add_at("https://a.test/", "", 1);
        assert_eq!(store.bookmarks()[0].title, "https://a.test/");
    }

    #[test]
    fn id로_지운다() {
        let mut store = empty_store();
        store.add_at("https://a.test/", "A", 1);
        store.add_at("https://b.test/", "B", 2);
        let id = store.bookmarks()[0].id.clone();
        store.remove(&id);
        assert_eq!(urls(&store), ["https://b.test/"]);
        store.remove("no-such-id");
        assert_eq!(urls(&store), ["https://b.test/"]);
    }

    #[test]
    fn url로_지운다() {
        let mut store = empty_store();
        store.add_at("https://a.test/", "A", 1);
        store.add_at("https://b.test/", "B", 2);
        store.remove_by_url("https://b.test/");
        assert_eq!(urls(&store), ["https://a.test/"]);
        store.remove_by_url("https://zzz.test/");
        assert_eq!(urls(&store), ["https://a.test/"]);
    }

    #[test]
    fn 바뀔_때마다_zustand와_같은_키와_모양으로_저장한다() {
        let shared = SharedStorage::default();
        let mut store = BookmarksStore::new(Box::new(shared.clone()));
        store.add_at("https://a.test/", "A", 1_700_000_000_000);
        let id = store.bookmarks()[0].id.clone();
        let stored: serde_json::Value =
            serde_json::from_str(&shared.get("rev:bookmarks").unwrap()).unwrap();
        assert_eq!(
            stored,
            json!({
                "state": { "bookmarks": [{
                    "id": id,
                    "url": "https://a.test/",
                    "title": "A",
                    "addedAt": 1_700_000_000_000u64
                }] },
                "version": 0
            })
        );

        store.remove(&id);
        assert_eq!(
            shared.get(STORAGE_KEY).as_deref(),
            Some(r#"{"state":{"bookmarks":[]},"version":0}"#)
        );
    }

    #[test]
    fn 아무것도_바꾸지_않는_동작도_저장은_한다() {
        // zustand persist는 set이 불릴 때마다 쓴다.
        let shared = SharedStorage::default();
        let mut store = BookmarksStore::new(Box::new(shared.clone()));
        assert_eq!(shared.get(STORAGE_KEY), None);
        store.remove_by_url("https://a.test/");
        assert_eq!(
            shared.get(STORAGE_KEY).as_deref(),
            Some(r#"{"state":{"bookmarks":[]},"version":0}"#)
        );
    }

    #[test]
    fn 저장된_북마크에서_다시_시작한다() {
        let shared = SharedStorage::default();
        let mut first = BookmarksStore::new(Box::new(shared.clone()));
        first.add_at("https://a.test/", "A", 10);
        first.add_at("https://b.test/", "B", 20);
        let second = BookmarksStore::new(Box::new(shared));
        assert_eq!(second.bookmarks(), first.bookmarks());
    }

    #[test]
    fn electron_앱이_쓴_json을_읽는다() {
        let store = store_from(
            r#"{"state":{"bookmarks":[{"id":"blx1abc123","url":"https://a.test/","title":"A","addedAt":1700000000000}]},"version":0}"#,
        );
        assert_eq!(
            store.bookmarks(),
            [Bookmark {
                id: "blx1abc123".into(),
                url: "https://a.test/".into(),
                title: "A".into(),
                added_at: 1_700_000_000_000,
            }]
        );
    }

    #[test]
    fn 깨진_저장값은_빈_목록으로_시작한다() {
        assert!(store_from("{oops").bookmarks().is_empty());
        assert!(store_from(r#"{"state":{}}"#).bookmarks().is_empty());
        assert!(
            store_from(r#"{"state":{"bookmarks":[{"id":"b1","url":"u","title":"t","addedAt":1}]},"version":3}"#)
                .bookmarks()
                .is_empty()
        );
    }

    #[test]
    fn 깨진_항목만_건너뛴다() {
        let store = store_from(
            r#"{"state":{"bookmarks":[null,{"id":"b1","url":"https://a.test/","title":"A","addedAt":1},{"id":7}]},"version":0}"#,
        );
        assert_eq!(urls(&store), ["https://a.test/"]);
    }
}
