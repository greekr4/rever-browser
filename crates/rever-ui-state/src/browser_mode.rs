//! Port of `src/renderer/src/stores/browser-mode.ts` — whether pages load in
//! the embedded view or an external browser, remembered across restarts.

use std::io;

use serde::{Deserialize, Serialize};

use crate::storage::{Storage, field, load_persisted, save_persisted};

const NAME: &str = "rev:browser-mode";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserMode {
    #[default]
    Embedded,
    External,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BrowserModeStore {
    mode: BrowserMode,
}

impl BrowserModeStore {
    pub fn load(storage: &dyn Storage) -> Self {
        let state = load_persisted(storage, NAME);
        Self {
            mode: field(&state, "mode").unwrap_or_default(),
        }
    }

    pub fn mode(&self) -> BrowserMode {
        self.mode
    }

    /// The state changes even when the write fails, as with `persist`.
    pub fn set_mode(&mut self, storage: &dyn Storage, mode: BrowserMode) -> io::Result<()> {
        self.mode = mode;
        save_persisted(storage, NAME, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn 기본은_embedded다() {
        assert_eq!(
            BrowserModeStore::load(&MemoryStorage::new()).mode(),
            BrowserMode::Embedded
        );
    }

    #[test]
    fn 모드를_persist_형식으로_저장하고_다시_불러온다() {
        let storage = MemoryStorage::new();
        let mut store = BrowserModeStore::load(&storage);
        store.set_mode(&storage, BrowserMode::External).unwrap();
        assert_eq!(
            storage.get("rev:browser-mode").as_deref(),
            Some(r#"{"state":{"mode":"external"},"version":0}"#)
        );
        assert_eq!(
            BrowserModeStore::load(&storage).mode(),
            BrowserMode::External
        );
        store.set_mode(&storage, BrowserMode::Embedded).unwrap();
        assert_eq!(
            BrowserModeStore::load(&storage).mode(),
            BrowserMode::Embedded
        );
    }

    #[test]
    fn 알_수_없는_모드는_기본값으로_둔다() {
        let storage = MemoryStorage::new();
        storage
            .set(
                "rev:browser-mode",
                r#"{"state":{"mode":"headless"},"version":0}"#,
            )
            .unwrap();
        assert_eq!(
            BrowserModeStore::load(&storage).mode(),
            BrowserMode::Embedded
        );
    }
}
