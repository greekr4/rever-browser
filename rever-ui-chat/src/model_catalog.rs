//! Port of `src/renderer/src/stores/model-catalog.ts` — the models each agent offers:
//! the live list its own CLI reported when there is one, else the hardcoded fallback.
//!
//! The zustand store and the module's `started` flag are the [`ModelCatalogStore`]
//! struct. `window.rev.models` is the [`ModelsHost`] the store is started with; its
//! answers arrive later, through [`ModelCatalogStore::put`].

use rever_agent::model_catalog_parse::{AgentCatalog, CatalogEntry};
use rever_ui_state::constants::{ACP_AGENTS, AcpAgentId, MODEL_CATALOG};
use serde::{Deserialize, Serialize};

/// A live model list read from an agent's own CLI (`ModelCatalog` in the preload).
pub type ModelCatalog = AgentCatalog;

/// One row of a model list: a catalog model, plus what a live list adds to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveModel {
    pub agent_id: AcpAgentId,
    pub model_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_levels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_fast_mode: Option<bool>,
}

impl LiveModel {
    fn live(agent_id: AcpAgentId, entry: &CatalogEntry) -> Self {
        Self {
            agent_id,
            model_id: entry.model_id.clone(),
            name: entry.name.clone(),
            description: entry.description.clone(),
            effort_levels: entry.effort_levels.clone(),
            supports_fast_mode: entry.supports_fast_mode,
        }
    }
}

fn fallback(agent_id: Option<AcpAgentId>) -> impl Iterator<Item = LiveModel> {
    MODEL_CATALOG
        .iter()
        .filter(move |m| agent_id.is_none_or(|id| m.agent_id == id))
        .map(|m| LiveModel {
            agent_id: m.agent_id,
            model_id: m.model_id.to_owned(),
            name: m.name.to_owned(),
            description: None,
            effort_levels: None,
            supports_fast_mode: None,
        })
}

/// `window.rev.models`, as far as this store uses it.
pub trait ModelsHost {
    /// `models.catalog(agentId)`: ask the main process for the agent's list. The answer
    /// (a catalog or nothing) goes to [`ModelCatalogStore::put`] when it arrives.
    fn request_catalog(&self, agent_id: AcpAgentId);
    /// `models.onUpdated(put)`: from now on every refreshed list goes to
    /// [`ModelCatalogStore::put`] as well.
    fn subscribe_updates(&self);
}

/// The agents whose CLIs report a model list.
const LIVE_AGENTS: [AcpAgentId; 2] = [AcpAgentId::ClaudeCode, AcpAgentId::Codex];

// Model lists read from the agents' own CLIs by the main process (cached on
// disk, refreshed each launch). Agents without a live list use MODEL_CATALOG.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelCatalogStore {
    /// In the order the agents first reported, as the keys of the JavaScript object were.
    live: Vec<(AcpAgentId, Vec<CatalogEntry>)>,
    started: bool,
}

impl ModelCatalogStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for the live lists and subscribe to their updates. Only the first call does
    /// anything; the React hooks called this on every render.
    pub fn start(&mut self, host: &dyn ModelsHost) {
        if self.started {
            return;
        }
        self.started = true;
        for id in LIVE_AGENTS {
            host.request_catalog(id);
        }
        host.subscribe_updates();
    }

    /// A list arrived. A missing or empty one changes nothing, so a failed refresh does
    /// not take a good list away. Returns whether the store changed.
    ///
    /// An id that is not one of the app's agents is dropped here; the TypeScript stored
    /// it, and the picker then skipped its rows.
    pub fn put(&mut self, agent_id: &str, catalog: Option<&ModelCatalog>) -> bool {
        let Some(catalog) = catalog.filter(|c| !c.models.is_empty()) else {
            return false;
        };
        let Some(id) = ACP_AGENTS
            .iter()
            .map(|agent| agent.id)
            .find(|id| id.as_str() == agent_id)
        else {
            return false;
        };
        match self.live.iter_mut().find(|(known, _)| *known == id) {
            Some((_, models)) => *models = catalog.models.clone(),
            None => self.live.push((id, catalog.models.clone())),
        }
        true
    }

    /// An agent's models: the live CLI list when we have one, else the hardcoded fallback.
    pub fn agent_models(&self, agent_id: AcpAgentId) -> Vec<LiveModel> {
        match self.live.iter().find(|(id, _)| *id == agent_id) {
            Some((_, models)) => models
                .iter()
                .map(|m| LiveModel::live(agent_id, m))
                .collect(),
            None => fallback(Some(agent_id)).collect(),
        }
    }

    /// Every agent's models (for the new-tab picker): the fallback rows of the agents
    /// without a live list first, then the live lists.
    pub fn all_agent_models(&self) -> Vec<LiveModel> {
        fallback(None)
            .filter(|m| !self.live.iter().any(|(id, _)| *id == m.agent_id))
            .chain(
                self.live
                    .iter()
                    .flat_map(|(id, models)| models.iter().map(|m| LiveModel::live(*id, m))),
            )
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use serde_json::json;

    use super::*;

    #[derive(Default)]
    struct FakeHost {
        calls: RefCell<Vec<String>>,
    }

    impl ModelsHost for FakeHost {
        fn request_catalog(&self, agent_id: AcpAgentId) {
            self.calls
                .borrow_mut()
                .push(format!("catalog:{}", agent_id.as_str()));
        }

        fn subscribe_updates(&self) {
            self.calls.borrow_mut().push("onUpdated".to_owned());
        }
    }

    fn catalog(models: serde_json::Value) -> ModelCatalog {
        serde_json::from_value(json!({ "models": models, "fetchedAt": "2026-01-01T00:00:00Z" }))
            .unwrap()
    }

    fn names(models: &[LiveModel]) -> Vec<&str> {
        models.iter().map(|m| m.name.as_str()).collect()
    }

    #[test]
    fn 시작하면_두_cli_에이전트의_목록을_요청하고_갱신을_구독한다() {
        let host = FakeHost::default();
        let mut store = ModelCatalogStore::new();
        store.start(&host);
        assert_eq!(
            *host.calls.borrow(),
            ["catalog:claude-code", "catalog:codex", "onUpdated"]
        );
    }

    #[test]
    fn 시작은_한_번만_한다() {
        let host = FakeHost::default();
        let mut store = ModelCatalogStore::new();
        store.start(&host);
        store.start(&host);
        assert_eq!(host.calls.borrow().len(), 3);
    }

    #[test]
    fn 라이브_목록이_없으면_하드코딩된_카탈로그를_쓴다() {
        let store = ModelCatalogStore::new();
        assert_eq!(
            names(&store.agent_models(AcpAgentId::ClaudeCode)),
            ["Default", "Fable", "Opus", "Sonnet", "Haiku"]
        );
        let all = store.all_agent_models();
        assert_eq!(all.len(), MODEL_CATALOG.len());
        assert_eq!(all[0].agent_id, AcpAgentId::ClaudeCode);
        assert_eq!(all[0].model_id, "default");
        assert_eq!(all[0].description, None);
    }

    #[test]
    fn 라이브_목록이_오면_그_에이전트의_모델은_그_목록이다() {
        let mut store = ModelCatalogStore::new();
        let live = catalog(json!([
            { "modelId": "opus[1m]", "name": "Opus 1M", "description": "Long context",
              "effortLevels": ["low", "high"], "supportsFastMode": true },
            { "modelId": "haiku", "name": "Haiku", "description": null }
        ]));
        assert!(store.put("claude-code", Some(&live)));
        assert_eq!(
            store.agent_models(AcpAgentId::ClaudeCode),
            vec![
                LiveModel {
                    agent_id: AcpAgentId::ClaudeCode,
                    model_id: "opus[1m]".into(),
                    name: "Opus 1M".into(),
                    description: Some("Long context".into()),
                    effort_levels: Some(vec!["low".into(), "high".into()]),
                    supports_fast_mode: Some(true),
                },
                LiveModel {
                    agent_id: AcpAgentId::ClaudeCode,
                    model_id: "haiku".into(),
                    name: "Haiku".into(),
                    description: None,
                    effort_levels: None,
                    supports_fast_mode: None,
                },
            ]
        );
        // The other agents keep their fallback rows.
        assert_eq!(
            names(&store.agent_models(AcpAgentId::Codex)),
            ["GPT-5 Codex", "GPT-5"]
        );
    }

    #[test]
    fn 없는_목록과_빈_목록은_무시한다() {
        let mut store = ModelCatalogStore::new();
        assert!(store.put(
            "codex",
            Some(&catalog(json!([{ "modelId": "gpt-6", "name": "GPT-6" }])))
        ));
        assert!(!store.put("codex", None));
        assert!(!store.put("codex", Some(&catalog(json!([])))));
        assert_eq!(names(&store.agent_models(AcpAgentId::Codex)), ["GPT-6"]);
    }

    #[test]
    fn 모르는_에이전트_id의_목록은_버린다() {
        let mut store = ModelCatalogStore::new();
        let live = catalog(json!([{ "modelId": "x", "name": "X" }]));
        assert!(!store.put("gemini", Some(&live)));
        assert!(!store.put("", Some(&live)));
        assert_eq!(store, ModelCatalogStore::new());
    }

    #[test]
    fn 전체_목록은_라이브가_없는_에이전트의_카탈로그_다음에_라이브_목록을_도착한_순서로_붙인다() {
        let mut store = ModelCatalogStore::new();
        store.put(
            "codex",
            Some(&catalog(json!([{ "modelId": "gpt-6", "name": "GPT-6" }]))),
        );
        store.put(
            "claude-code",
            Some(&catalog(json!([{ "modelId": "opus", "name": "Opus" }]))),
        );
        // A refresh replaces the list but keeps the agent's place.
        store.put(
            "codex",
            Some(&catalog(json!([
                { "modelId": "gpt-6", "name": "GPT-6" },
                { "modelId": "gpt-6-mini", "name": "GPT-6 mini" }
            ]))),
        );
        let all = store.all_agent_models();
        let fallback_rows = MODEL_CATALOG
            .iter()
            .filter(|m| !LIVE_AGENTS.contains(&m.agent_id))
            .count();
        assert_eq!(all.len(), fallback_rows + 3);
        assert_eq!(all[0].name, "Claude Opus 4.8");
        assert_eq!(
            names(&all[fallback_rows..]),
            ["GPT-6", "GPT-6 mini", "Opus"]
        );
        assert_eq!(all[fallback_rows].agent_id, AcpAgentId::Codex);
        assert_eq!(all[fallback_rows + 2].agent_id, AcpAgentId::ClaudeCode);
    }

    #[test]
    fn 모델은_camelcase로_직렬화되고_없는_필드는_빠진다() {
        let store = ModelCatalogStore::new();
        let first = store.agent_models(AcpAgentId::Codex).remove(0);
        assert_eq!(
            json!(first),
            json!({ "agentId": "codex", "modelId": "gpt-5-codex", "name": "GPT-5 Codex" })
        );
    }
}
