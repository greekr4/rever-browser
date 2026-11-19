//! Port of `src/renderer/src/stores/notify.ts` — whether the app shows OS
//! notifications (e.g. when a Red Team scan finishes).
//!
//! Persisted so the choice survives restarts; defaults to on.

use crate::storage::Storage;

const KEY: &str = "rev:notify";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyStore {
    enabled: bool,
}

impl NotifyStore {
    pub fn load(storage: &dyn Storage) -> Self {
        Self {
            enabled: storage.get(KEY).as_deref() != Some("off"),
        }
    }

    /// Also the read for code outside the interface (the scan fires notifications).
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, storage: &dyn Storage, v: bool) {
        // A failed write only loses the choice across a restart; the setting
        // still applies now.
        let _ = storage.set(KEY, if v { "on" } else { "off" });
        self.enabled = v;
    }

    pub fn toggle(&mut self, storage: &dyn Storage) {
        self.set_enabled(storage, !self.enabled);
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use crate::storage::MemoryStorage;

    struct FailingStorage;

    impl Storage for FailingStorage {
        fn get(&self, _key: &str) -> Option<String> {
            None
        }
        fn set(&self, _key: &str, _value: &str) -> io::Result<()> {
            Err(io::Error::other("quota exceeded"))
        }
        fn remove(&self, _key: &str) -> io::Result<()> {
            Ok(())
        }
    }

    // --- notify.test.ts ---

    #[test]
    fn 기본값은_켜짐이다() {
        let store = NotifyStore::load(&MemoryStorage::new());
        assert!(store.enabled());
    }

    #[test]
    fn toggle이_상태를_뒤집고_저장소에_저장한다() {
        let storage = MemoryStorage::new();
        let mut store = NotifyStore::load(&storage);

        store.toggle(&storage);
        assert!(!store.enabled());
        assert_eq!(storage.get("rev:notify").as_deref(), Some("off"));

        store.toggle(&storage);
        assert!(store.enabled());
        assert_eq!(storage.get("rev:notify").as_deref(), Some("on"));
    }

    // --- not covered by notify.test.ts ---

    #[test]
    fn 저장된_값이_off일_때만_꺼진_채로_불러온다() {
        let storage = MemoryStorage::new();
        storage.set("rev:notify", "off").unwrap();
        assert!(!NotifyStore::load(&storage).enabled());
        for other in ["on", "", "OFF", "false"] {
            storage.set("rev:notify", other).unwrap();
            assert!(NotifyStore::load(&storage).enabled(), "{other:?}");
        }
    }

    #[test]
    fn 저장에_실패해도_상태는_바뀐다() {
        let mut store = NotifyStore::load(&FailingStorage);
        store.set_enabled(&FailingStorage, false);
        assert!(!store.enabled());
    }
}
