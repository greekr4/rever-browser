//! Port of `src/renderer/src/stores/chat-collapsed.ts` — whether the chat
//! pane is collapsed, remembered across restarts.

use std::io;

use serde::Serialize;

use crate::storage::{Storage, field, load_persisted, save_persisted};

const NAME: &str = "rev:chat-collapsed";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChatCollapsedStore {
    collapsed: bool,
}

impl ChatCollapsedStore {
    pub fn load(storage: &dyn Storage) -> Self {
        let state = load_persisted(storage, NAME);
        Self {
            collapsed: field(&state, "collapsed").unwrap_or(false),
        }
    }

    pub fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// The state changes even when the write fails, as with `persist`.
    pub fn set_collapsed(&mut self, storage: &dyn Storage, collapsed: bool) -> io::Result<()> {
        self.collapsed = collapsed;
        save_persisted(storage, NAME, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn 기본은_펼친_상태다() {
        assert!(!ChatCollapsedStore::load(&MemoryStorage::new()).collapsed());
    }

    #[test]
    fn 접힘_상태를_persist_형식으로_저장하고_다시_불러온다() {
        let storage = MemoryStorage::new();
        let mut store = ChatCollapsedStore::load(&storage);
        store.set_collapsed(&storage, true).unwrap();
        assert_eq!(
            storage.get("rev:chat-collapsed").as_deref(),
            Some(r#"{"state":{"collapsed":true},"version":0}"#)
        );
        assert!(ChatCollapsedStore::load(&storage).collapsed());
        store.set_collapsed(&storage, false).unwrap();
        assert!(!ChatCollapsedStore::load(&storage).collapsed());
    }

    #[test]
    fn 깨진_저장값이면_기본값으로_시작한다() {
        let storage = MemoryStorage::new();
        for raw in ["{", r#"{"state":{"collapsed":"yes"},"version":0}"#] {
            storage.set("rev:chat-collapsed", raw).unwrap();
            assert!(!ChatCollapsedStore::load(&storage).collapsed(), "{raw}");
        }
    }
}
