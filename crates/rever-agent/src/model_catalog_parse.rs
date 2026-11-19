//! Port of `src/main/model-catalog-parse.ts` — pure parsing for the live model catalog.
//! The lists come from the agents' own CLIs, so they follow CLI updates the way
//! Orca's do — the probes and the cache live with the catalog itself.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub model_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_levels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_fast_mode: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCatalog {
    pub models: Vec<CatalogEntry>,
    pub fetched_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogFile {
    /// Always 1.
    pub version: u8,
    pub agents: BTreeMap<String, AgentCatalog>,
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str()
}

fn array_field<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn is_true(v: &Value, key: &str) -> bool {
    v.get(key) == Some(&Value::Bool(true))
}

/// Claude Code `initialize` control_response → catalog rows.
pub fn claude_models_from_init(init: &Value) -> Vec<CatalogEntry> {
    array_field(init, "models")
        .iter()
        .filter_map(|m| {
            let value = str_field(m, "value")?;
            Some(CatalogEntry {
                model_id: value.to_owned(),
                name: str_field(m, "displayName").unwrap_or(value).to_owned(),
                description: str_field(m, "description").map(str::to_owned),
                effort_levels: Some(
                    array_field(m, "supportedEffortLevels")
                        .iter()
                        .filter_map(|level| level.as_str().map(str::to_owned))
                        .collect(),
                ),
                supports_fast_mode: Some(is_true(m, "supportsFastMode")),
            })
        })
        .collect()
}

/// `codex app-server` model/list result page → catalog rows (hidden models dropped, default first).
pub fn codex_models_from_list(result: &Value) -> Vec<CatalogEntry> {
    let mut data: Vec<&Value> = array_field(result, "data")
        .iter()
        .filter(|m| str_field(m, "id").is_some() && !is_true(m, "hidden"))
        .collect();
    // Stable, like Array.prototype.sort: the rest keep the order the CLI listed them in.
    data.sort_by_key(|m| !is_true(m, "isDefault"));
    data.into_iter()
        .filter_map(|m| {
            let id = str_field(m, "id")?;
            Some(CatalogEntry {
                model_id: id.to_owned(),
                name: str_field(m, "displayName").unwrap_or(id).to_owned(),
                description: str_field(m, "description").map(str::to_owned),
                effort_levels: None,
                supports_fast_mode: None,
            })
        })
        .collect()
}

/// Reject a cache file whose shape we don't recognise instead of trusting it.
pub fn parse_catalog_file(raw: &str) -> CatalogFile {
    let mut agents = BTreeMap::new();
    if let Ok(v) = serde_json::from_str::<Value>(raw)
        && v.get("version").and_then(Value::as_f64) == Some(1.0)
    {
        let entries: Vec<(String, &Value)> = match v.get("agents") {
            Some(Value::Object(map)) => map.iter().map(|(id, c)| (id.clone(), c)).collect(),
            // `typeof [] === 'object'`: the TypeScript takes an array too, keyed by index.
            Some(Value::Array(items)) => items
                .iter()
                .enumerate()
                .map(|(index, c)| (index.to_string(), c))
                .collect(),
            _ => Vec::new(),
        };
        for (id, c) in entries {
            if let Some(models) = c.get("models").and_then(Value::as_array)
                && let Some(fetched_at) = str_field(c, "fetchedAt")
            {
                agents.insert(
                    id,
                    AgentCatalog {
                        models: models
                            .iter()
                            .filter_map(|m| CatalogEntry::deserialize(m).ok())
                            .collect(),
                        fetched_at: fetched_at.to_owned(),
                    },
                );
            }
        }
    }
    CatalogFile { version: 1, agents }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // ── claudeModelsFromInit ──

    #[test]
    fn initialize_응답의_모델을_이름_설명_추론_수준_빠른_모드와_함께_옮긴다() {
        let init = json!({
            "models": [
                { "value": "default", "displayName": "Default (recommended)", "description": "Fable 5.1", "supportedEffortLevels": ["low", "high"] },
                { "value": "opus", "displayName": "Opus 5.5", "supportsFastMode": true },
                { "displayName": "no value — dropped" }
            ]
        });
        assert_eq!(
            serde_json::to_value(claude_models_from_init(&init)).unwrap(),
            json!([
                { "modelId": "default", "name": "Default (recommended)", "description": "Fable 5.1", "effortLevels": ["low", "high"], "supportsFastMode": false },
                { "modelId": "opus", "name": "Opus 5.5", "description": null, "effortLevels": [], "supportsFastMode": true }
            ])
        );
    }

    #[test]
    fn 모델_목록이_없으면_빈_배열() {
        assert_eq!(claude_models_from_init(&json!({})), []);
    }

    // ── codexModelsFromList ──

    #[test]
    fn 숨김_모델을_빼고_기본_모델을_맨_앞에_둔다() {
        let res = json!({
            "data": [
                { "id": "gpt-6-sol", "displayName": "GPT-6-Sol", "description": "b", "hidden": false, "isDefault": false },
                { "id": "gpt-old", "displayName": "Old", "hidden": true },
                { "id": "gpt-6.1-sol", "displayName": "GPT-6.1-Sol", "description": "a", "hidden": false, "isDefault": true }
            ],
            "nextCursor": null
        });
        let models = codex_models_from_list(&res);
        let ids: Vec<&str> = models.iter().map(|m| m.model_id.as_str()).collect();
        assert_eq!(ids, ["gpt-6.1-sol", "gpt-6-sol"]);
        assert_eq!(
            serde_json::to_value(&models[0]).unwrap(),
            json!({ "modelId": "gpt-6.1-sol", "name": "GPT-6.1-Sol", "description": "a" })
        );
    }

    // ── parseCatalogFile ──

    #[test]
    fn 알아보는_형식만_받아들이고_나머지는_빈_캐시로_둔다() {
        let ok = json!({
            "version": 1,
            "agents": {
                "codex": { "models": [{ "modelId": "x", "name": "X" }], "fetchedAt": "2026-11-06T00:00:00Z" },
                "bad": { "models": "nope" }
            }
        });
        let parsed = parse_catalog_file(&ok.to_string());
        assert_eq!(parsed.agents.keys().collect::<Vec<_>>(), ["codex"]);
        assert!(parse_catalog_file(r#"{"version":2}"#).agents.is_empty());
        assert!(parse_catalog_file("not json").agents.is_empty());
    }

    // Not in the TypeScript tests: `Object.entries` walks an array by index.
    #[test]
    fn agents가_배열이어도_인덱스를_키로_받아들인다() {
        let parsed = parse_catalog_file(
            r#"{"version":1.0,"agents":[{"models":[{"modelId":"x","name":"X"}],"fetchedAt":"t"},{"models":[]},null]}"#,
        );
        assert_eq!(parsed.agents.keys().collect::<Vec<_>>(), ["0"]);
        assert_eq!(parsed.agents["0"].models[0].model_id, "x");
        assert!(
            parse_catalog_file(r#"{"version":1,"agents":"x"}"#)
                .agents
                .is_empty()
        );
        assert!(
            parse_catalog_file(r#"{"version":"1","agents":{}}"#)
                .agents
                .is_empty()
        );
    }
}
