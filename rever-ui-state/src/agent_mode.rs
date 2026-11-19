//! Port of `src/renderer/src/stores/agent-mode.ts` — which agent surface the
//! right-hand pane shows.

use std::io;

use serde::{Deserialize, Serialize};

use crate::storage::Storage;

/// - `Acp`: the structured in-app chat over the ACP transport (default)
/// - `Cli`: a real terminal running a local CLI agent (Claude Code) wired to
///   rever's MCP server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    #[default]
    Acp,
    Cli,
}

impl AgentMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Acp => "acp",
            Self::Cli => "cli",
        }
    }
}

const KEY: &str = "rev:agent-mode";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentModeStore {
    mode: AgentMode,
}

impl AgentModeStore {
    /// The value is stored bare (`acp` / `cli`), not as JSON. Anything else
    /// reads as the default.
    pub fn load(storage: &dyn Storage) -> Self {
        let mode = match storage.get(KEY).as_deref() {
            Some("cli") => AgentMode::Cli,
            _ => AgentMode::Acp,
        };
        Self { mode }
    }

    pub fn mode(&self) -> AgentMode {
        self.mode
    }

    /// Nothing changes when the write fails: the write comes first, as in the
    /// TypeScript, where a throwing `setItem` skipped the state update.
    pub fn set_mode(&mut self, storage: &dyn Storage, mode: AgentMode) -> io::Result<()> {
        storage.set(KEY, mode.as_str())?;
        self.mode = mode;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn 저장값이_없거나_비어_있으면_acp다() {
        let storage = MemoryStorage::new();
        assert_eq!(AgentModeStore::load(&storage).mode(), AgentMode::Acp);
        storage.set("rev:agent-mode", "").unwrap();
        assert_eq!(AgentModeStore::load(&storage).mode(), AgentMode::Acp);
    }

    #[test]
    fn set_mode는_따옴표_없는_문자열로_저장하고_다시_불러온다() {
        let storage = MemoryStorage::new();
        let mut store = AgentModeStore::load(&storage);
        store.set_mode(&storage, AgentMode::Cli).unwrap();
        assert_eq!(store.mode(), AgentMode::Cli);
        assert_eq!(storage.get("rev:agent-mode").as_deref(), Some("cli"));
        assert_eq!(AgentModeStore::load(&storage).mode(), AgentMode::Cli);
        store.set_mode(&storage, AgentMode::Acp).unwrap();
        assert_eq!(storage.get("rev:agent-mode").as_deref(), Some("acp"));
    }

    #[test]
    fn 알_수_없는_저장값은_acp로_읽는다() {
        let storage = MemoryStorage::new();
        for raw in ["terminal", "CLI", "\"cli\""] {
            storage.set("rev:agent-mode", raw).unwrap();
            assert_eq!(
                AgentModeStore::load(&storage).mode(),
                AgentMode::Acp,
                "{raw}"
            );
        }
    }

    #[test]
    fn 저장에_실패하면_모드를_바꾸지_않는다() {
        let mut store = AgentModeStore::load(&FailingStorage);
        assert!(store.set_mode(&FailingStorage, AgentMode::Cli).is_err());
        assert_eq!(store.mode(), AgentMode::Acp);
    }
}
