//! Port of `src/renderer/src/stores/agent-choice.ts` — the agent picked during
//! first-run onboarding.
//!
//! The chat panel seeds its default from `read_agent_choice` at start and
//! watches `choice` so a pick made in the onboarding modal switches the live
//! chat without a reload.

use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::constants::AcpAgentId;
use crate::storage::Storage;

const KEY: &str = "rev:agent-onboarded";
const SKIPPED: &str = "skipped";

/// The stored pick (`Stored` in the TypeScript).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentChoice {
    pub id: AcpAgentId,
    /// Written as `null` when there is none, so the key is always present.
    pub path: Option<String>,
}

/// Read the persisted pick. Returns `None` on first run or after a skip.
pub fn read_agent_choice(storage: &dyn Storage) -> Option<AgentChoice> {
    let raw = storage.get(KEY)?;
    if raw.is_empty() || raw == SKIPPED {
        return None;
    }
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    Some(AgentChoice {
        id: AcpAgentId::deserialize(parsed.get("id")?).ok()?,
        path: parsed
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// True once the user has either picked an agent or skipped onboarding.
pub fn has_onboarded(storage: &dyn Storage) -> bool {
    storage.get(KEY).is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentChoiceStore {
    /// Mirrors `has_onboarded`; flips when the onboarding modal closes.
    onboarded: bool,
    /// Set only when the user picks during this session (`None` = untouched).
    choice: Option<AgentChoice>,
}

impl AgentChoiceStore {
    pub fn load(storage: &dyn Storage) -> Self {
        Self {
            onboarded: has_onboarded(storage),
            choice: None,
        }
    }

    pub fn onboarded(&self) -> bool {
        self.onboarded
    }

    pub fn choice(&self) -> Option<&AgentChoice> {
        self.choice.as_ref()
    }

    /// Nothing changes when the write fails: the write comes first, as in the
    /// TypeScript, where a throwing `setItem` skipped the state update.
    pub fn pick(
        &mut self,
        storage: &dyn Storage,
        id: AcpAgentId,
        path: Option<String>,
    ) -> io::Result<()> {
        let choice = AgentChoice { id, path };
        storage.set(KEY, &serde_json::to_string(&choice)?)?;
        self.choice = Some(choice);
        self.onboarded = true;
        Ok(())
    }

    pub fn skip(&mut self, storage: &dyn Storage) -> io::Result<()> {
        storage.set(KEY, SKIPPED)?;
        self.onboarded = true;
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
    fn 첫_실행에는_온보딩_전이고_선택이_없다() {
        let storage = MemoryStorage::new();
        assert!(!has_onboarded(&storage));
        assert_eq!(read_agent_choice(&storage), None);
        let store = AgentChoiceStore::load(&storage);
        assert!(!store.onboarded());
        assert_eq!(store.choice(), None);
    }

    #[test]
    fn pick은_id와_path를_json으로_저장하고_온보딩을_마친다() {
        let storage = MemoryStorage::new();
        let mut store = AgentChoiceStore::load(&storage);
        store
            .pick(
                &storage,
                AcpAgentId::ClaudeCode,
                Some("/usr/local/bin/claude".to_owned()),
            )
            .unwrap();
        assert_eq!(
            storage.get("rev:agent-onboarded").as_deref(),
            Some(r#"{"id":"claude-code","path":"/usr/local/bin/claude"}"#)
        );
        let expected = AgentChoice {
            id: AcpAgentId::ClaudeCode,
            path: Some("/usr/local/bin/claude".to_owned()),
        };
        assert!(store.onboarded());
        assert_eq!(store.choice(), Some(&expected));
        assert_eq!(read_agent_choice(&storage), Some(expected));
        assert!(has_onboarded(&storage));
    }

    #[test]
    fn path가_없으면_null로_저장한다() {
        let storage = MemoryStorage::new();
        let mut store = AgentChoiceStore::load(&storage);
        store.pick(&storage, AcpAgentId::Anthropic, None).unwrap();
        assert_eq!(
            storage.get("rev:agent-onboarded").as_deref(),
            Some(r#"{"id":"anthropic","path":null}"#)
        );
        assert_eq!(
            read_agent_choice(&storage),
            Some(AgentChoice {
                id: AcpAgentId::Anthropic,
                path: None
            })
        );
    }

    #[test]
    fn skip은_온보딩만_마치고_선택은_남기지_않는다() {
        let storage = MemoryStorage::new();
        let mut store = AgentChoiceStore::load(&storage);
        store.skip(&storage).unwrap();
        assert_eq!(
            storage.get("rev:agent-onboarded").as_deref(),
            Some("skipped")
        );
        assert!(store.onboarded());
        assert_eq!(store.choice(), None);
        assert_eq!(read_agent_choice(&storage), None);
        // The next launch sees onboarding as done, with no session pick.
        let next = AgentChoiceStore::load(&storage);
        assert!(next.onboarded());
        assert_eq!(next.choice(), None);
    }

    #[test]
    fn 깨진_저장값은_선택_없음이지만_온보딩은_마친_것으로_본다() {
        let storage = MemoryStorage::new();
        for raw in [
            "",
            "not json",
            "null",
            "5",
            "{}",
            r#"{"id":""}"#,
            r#"{"id":null,"path":"/x"}"#,
            r#"{"id":"gemini","path":null}"#,
        ] {
            storage.set("rev:agent-onboarded", raw).unwrap();
            assert_eq!(read_agent_choice(&storage), None, "{raw}");
            assert!(has_onboarded(&storage), "{raw}");
        }
    }

    #[test]
    fn 문자열이_아닌_path는_없는_것으로_읽는다() {
        let storage = MemoryStorage::new();
        storage
            .set("rev:agent-onboarded", r#"{"id":"codex","path":5}"#)
            .unwrap();
        assert_eq!(
            read_agent_choice(&storage),
            Some(AgentChoice {
                id: AcpAgentId::Codex,
                path: None
            })
        );
    }

    #[test]
    fn 저장에_실패하면_상태를_바꾸지_않는다() {
        let mut store = AgentChoiceStore::load(&FailingStorage);
        assert!(
            store
                .pick(&FailingStorage, AcpAgentId::Codex, None)
                .is_err()
        );
        assert!(store.skip(&FailingStorage).is_err());
        assert!(!store.onboarded());
        assert_eq!(store.choice(), None);
    }
}
