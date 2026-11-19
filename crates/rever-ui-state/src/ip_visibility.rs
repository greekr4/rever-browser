//! Port of `src/renderer/src/stores/ip-visibility.ts` — whether the titlebar
//! IpBadge shows the real egress IP or a masked placeholder.
//!
//! Persisted so it stays hidden across restarts once turned off for recording —
//! the IP still refreshes in the background, only the render is masked.

use std::io;

use serde::Serialize;

use crate::storage::{Storage, field, load_persisted, save_persisted};

const NAME: &str = "rev:ip-hidden";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IpVisibilityStore {
    hidden: bool,
}

impl IpVisibilityStore {
    pub fn load(storage: &dyn Storage) -> Self {
        let state = load_persisted(storage, NAME);
        Self {
            hidden: field(&state, "hidden").unwrap_or(false),
        }
    }

    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// The state changes even when the write fails, as with `persist`.
    pub fn toggle(&mut self, storage: &dyn Storage) -> io::Result<()> {
        self.hidden = !self.hidden;
        save_persisted(storage, NAME, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn 기본은_ip를_보여준다() {
        assert!(!IpVisibilityStore::load(&MemoryStorage::new()).hidden());
    }

    #[test]
    fn toggle은_상태를_뒤집고_재시작_뒤에도_유지된다() {
        let storage = MemoryStorage::new();
        let mut store = IpVisibilityStore::load(&storage);
        store.toggle(&storage).unwrap();
        assert!(store.hidden());
        assert_eq!(
            storage.get("rev:ip-hidden").as_deref(),
            Some(r#"{"state":{"hidden":true},"version":0}"#)
        );
        assert!(IpVisibilityStore::load(&storage).hidden());
        store.toggle(&storage).unwrap();
        assert!(!IpVisibilityStore::load(&storage).hidden());
    }

    #[test]
    fn 타입이_다른_저장값은_기본값으로_둔다() {
        let storage = MemoryStorage::new();
        storage
            .set("rev:ip-hidden", r#"{"state":{"hidden":1},"version":0}"#)
            .unwrap();
        assert!(!IpVisibilityStore::load(&storage).hidden());
    }
}
