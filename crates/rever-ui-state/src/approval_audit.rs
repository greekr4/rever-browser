//! Port of `src/renderer/src/stores/approval-audit.ts` — a session audit trail
//! of agent permission decisions, so a user can later trace which sensitive
//! action was approved (or denied) and when.
//!
//! Mirrors the ARTEX guard/ audit-trail idea, scaled to a single-agent store.

use std::io;

use serde::{Deserialize, Serialize};

use crate::random_suffix;
use crate::storage::{Storage, list_field, load_persisted, save_persisted};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalDecision {
    Allow,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalAuditEntry {
    /// Best-effort tool label (matched risky tool id, else the request title).
    pub tool: String,
    /// Target host extracted from the tool input, if any.
    pub target: Option<String>,
    /// Whether the decision allowed or rejected the action.
    pub decision: ApprovalDecision,
    /// True when resolved by auto-approve without showing the prompt.
    pub auto: bool,
    /// True when this was a forced prompt for a risky tool.
    pub risky: bool,
    pub id: String,
    pub ts: u64,
}

/// An entry before `record` gives it an id and a timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalAuditInput {
    pub tool: String,
    pub target: Option<String>,
    pub decision: ApprovalDecision,
    pub auto: bool,
    pub risky: bool,
}

const MAX_ENTRIES: usize = 200;

const NAME: &str = "rever-browser:approval-audit";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApprovalAuditStore {
    /// Newest first.
    entries: Vec<ApprovalAuditEntry>,
}

impl ApprovalAuditStore {
    pub fn load(storage: &dyn Storage) -> Self {
        let state = load_persisted(storage, NAME);
        Self {
            entries: list_field(&state, "entries").unwrap_or_default(),
        }
    }

    pub fn entries(&self) -> &[ApprovalAuditEntry] {
        &self.entries
    }

    /// The state changes even when the write fails, as with `persist`.
    pub fn add(&mut self, storage: &dyn Storage, entry: ApprovalAuditEntry) -> io::Result<()> {
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_ENTRIES);
        save_persisted(storage, NAME, self)
    }

    pub fn clear(&mut self, storage: &dyn Storage) -> io::Result<()> {
        self.entries.clear();
        save_persisted(storage, NAME, self)
    }

    /// `recordApprovalAudit`: `now_ms` stands in for `Date.now()` and `random`
    /// for `Math.random()` (a value in `[0, 1)`).
    pub fn record(
        &mut self,
        storage: &dyn Storage,
        input: ApprovalAuditInput,
        now_ms: u64,
        random: f64,
    ) -> io::Result<()> {
        self.add(
            storage,
            ApprovalAuditEntry {
                tool: input.tool,
                target: input.target,
                decision: input.decision,
                auto: input.auto,
                risky: input.risky,
                id: format!("{now_ms}-{}", random_suffix(random)),
                ts: now_ms,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn entry(id: &str) -> ApprovalAuditEntry {
        ApprovalAuditEntry {
            tool: "replay_request".to_owned(),
            target: Some("api.a.test".to_owned()),
            decision: ApprovalDecision::Allow,
            auto: false,
            risky: true,
            id: id.to_owned(),
            ts: 1_700_000_000_000,
        }
    }

    #[test]
    fn 처음에는_기록이_없다() {
        assert!(
            ApprovalAuditStore::load(&MemoryStorage::new())
                .entries()
                .is_empty()
        );
    }

    #[test]
    fn add는_최신_항목을_맨_앞에_넣는다() {
        let storage = MemoryStorage::new();
        let mut store = ApprovalAuditStore::load(&storage);
        store.add(&storage, entry("a")).unwrap();
        store.add(&storage, entry("b")).unwrap();
        let ids: Vec<&str> = store.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
    }

    #[test]
    fn 기록은_200개까지만_남기고_오래된_것부터_버린다() {
        let storage = MemoryStorage::new();
        let mut store = ApprovalAuditStore::load(&storage);
        for i in 0..205 {
            store.add(&storage, entry(&i.to_string())).unwrap();
        }
        assert_eq!(store.entries().len(), 200);
        assert_eq!(store.entries().first().unwrap().id, "204");
        assert_eq!(store.entries().last().unwrap().id, "5");
        assert_eq!(ApprovalAuditStore::load(&storage).entries().len(), 200);
    }

    #[test]
    fn persist_형식으로_저장하고_다시_불러온다() {
        let storage = MemoryStorage::new();
        let mut store = ApprovalAuditStore::load(&storage);
        store
            .add(
                &storage,
                ApprovalAuditEntry {
                    target: None,
                    decision: ApprovalDecision::Reject,
                    ..entry("a")
                },
            )
            .unwrap();
        assert_eq!(
            storage.get("rever-browser:approval-audit").as_deref(),
            Some(
                r#"{"state":{"entries":[{"tool":"replay_request","target":null,"decision":"reject","auto":false,"risky":true,"id":"a","ts":1700000000000}]},"version":0}"#
            )
        );
        assert_eq!(ApprovalAuditStore::load(&storage), store);
    }

    #[test]
    fn clear는_기록과_저장본을_비운다() {
        let storage = MemoryStorage::new();
        let mut store = ApprovalAuditStore::load(&storage);
        store.add(&storage, entry("a")).unwrap();
        store.clear(&storage).unwrap();
        assert!(store.entries().is_empty());
        assert_eq!(
            storage.get("rever-browser:approval-audit").as_deref(),
            Some(r#"{"state":{"entries":[]},"version":0}"#)
        );
    }

    #[test]
    fn record는_시각과_난수로_id를_만들어_넣는다() {
        let storage = MemoryStorage::new();
        let mut store = ApprovalAuditStore::load(&storage);
        store
            .record(
                &storage,
                ApprovalAuditInput {
                    tool: "browser_navigate".to_owned(),
                    target: None,
                    decision: ApprovalDecision::Allow,
                    auto: true,
                    risky: false,
                },
                1_700_000_000_000,
                0.123456789,
            )
            .unwrap();
        assert_eq!(
            store.entries(),
            [ApprovalAuditEntry {
                tool: "browser_navigate".to_owned(),
                target: None,
                decision: ApprovalDecision::Allow,
                auto: true,
                risky: false,
                // `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`
                id: "1700000000000-4fzzzx".to_owned(),
                ts: 1_700_000_000_000,
            }]
        );
    }

    #[test]
    fn 형식이_맞지_않는_저장_항목은_버리고_나머지는_불러온다() {
        let storage = MemoryStorage::new();
        storage
            .set(
                "rever-browser:approval-audit",
                r#"{"state":{"entries":[{"tool":"t","target":null,"decision":"allow","auto":false,"risky":false,"id":"ok","ts":1},{"tool":"t","decision":"maybe","id":"bad"},null,7]},"version":0}"#,
            )
            .unwrap();
        let store = ApprovalAuditStore::load(&storage);
        assert_eq!(store.entries().len(), 1);
        assert_eq!(store.entries()[0].id, "ok");

        storage
            .set(
                "rever-browser:approval-audit",
                r#"{"state":{"entries":"x"},"version":0}"#,
            )
            .unwrap();
        assert!(ApprovalAuditStore::load(&storage).entries().is_empty());
    }
}
