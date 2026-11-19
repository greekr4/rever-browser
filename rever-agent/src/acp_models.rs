//! Port of `src/main/acp-models.ts` — the model-picker state read from an ACP
//! `session/new` result, whichever way the agent exposes its models.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub model_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelState {
    pub available_models: Vec<ModelInfo>,
    pub current_model_id: Option<String>,
    /// Set when the agent exposes the model as a session config option (newer
    /// claude-agent-acp); switching then goes through setSessionConfigOption
    /// instead of the removed unstable_setSessionModel.
    pub model_config_id: Option<String>,
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

/// `result` is the agent's `session/new` response (`models` and/or `configOptions`).
pub fn model_state_from_session(result: &Value) -> SessionModelState {
    let option = array_field(result, "configOptions").iter().find(|o| {
        str_field(o, "category") == Some("model") && str_field(o, "type") == Some("select")
    });
    if let Some(option) = option {
        return SessionModelState {
            available_models: array_field(option, "options")
                .iter()
                .flat_map(|o| match o.get("group") {
                    Some(_) => array_field(o, "options"),
                    None => std::slice::from_ref(o),
                })
                .filter_map(|o| {
                    Some(ModelInfo {
                        model_id: str_field(o, "value")?.to_owned(),
                        name: str_field(o, "name")?.to_owned(),
                        description: str_field(o, "description").map(str::to_owned),
                    })
                })
                .collect(),
            current_model_id: str_field(option, "currentValue").map(str::to_owned),
            model_config_id: str_field(option, "id").map(str::to_owned),
        };
    }
    let models = result.get("models");
    SessionModelState {
        available_models: models
            .map(|m| array_field(m, "availableModels"))
            .unwrap_or_default()
            .iter()
            .filter_map(|m| ModelInfo::deserialize(m).ok())
            .collect(),
        current_model_id: models
            .and_then(|m| str_field(m, "currentModelId"))
            .map(str::to_owned),
        model_config_id: None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn model(model_id: &str, name: &str, description: Option<&str>) -> ModelInfo {
        ModelInfo {
            model_id: model_id.to_owned(),
            name: name.to_owned(),
            description: description.map(str::to_owned),
        }
    }

    #[test]
    fn 구버전_에이전트의_models_필드를_그대로_쓴다() {
        let state = model_state_from_session(&json!({
            "models": {
                "availableModels": [{ "modelId": "opus", "name": "Opus" }],
                "currentModelId": "opus"
            }
        }));
        assert_eq!(
            state,
            SessionModelState {
                available_models: vec![model("opus", "Opus", None)],
                current_model_id: Some("opus".to_owned()),
                model_config_id: None,
            }
        );
    }

    #[test]
    fn 신버전_에이전트는_category가_model인_config_option에서_모델_목록을_만든다() {
        let state = model_state_from_session(&json!({
            "configOptions": [
                { "id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": "default", "options": [] },
                {
                    "id": "model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "opus",
                    "options": [
                        { "value": "default", "name": "Default", "description": "Recommended" },
                        { "value": "opus", "name": "Opus" }
                    ]
                }
            ]
        }));
        assert_eq!(
            serde_json::to_value(state).unwrap(),
            json!({
                "availableModels": [
                    { "modelId": "default", "name": "Default", "description": "Recommended" },
                    { "modelId": "opus", "name": "Opus", "description": null }
                ],
                "currentModelId": "opus",
                "modelConfigId": "model"
            })
        );
    }

    #[test]
    fn 그룹으로_묶인_옵션은_펼친다() {
        let state = model_state_from_session(&json!({
            "configOptions": [
                {
                    "id": "model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "b",
                    "options": [
                        { "group": "g1", "name": "G1", "options": [{ "value": "a", "name": "A" }] },
                        { "group": "g2", "name": "G2", "options": [{ "value": "b", "name": "B" }] }
                    ]
                }
            ]
        }));
        let ids: Vec<&str> = state
            .available_models
            .iter()
            .map(|m| m.model_id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(state.current_model_id.as_deref(), Some("b"));
    }

    #[test]
    fn 모델_정보가_없으면_빈_상태를_돌려준다() {
        assert_eq!(
            serde_json::to_value(model_state_from_session(&json!({}))).unwrap(),
            json!({ "availableModels": [], "currentModelId": null, "modelConfigId": null })
        );
    }
}
