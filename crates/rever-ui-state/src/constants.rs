//! Port of `src/renderer/src/constants.ts` — the agents the picker offers and
//! the models each one routes to.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AcpAgentId {
    ClaudeCode,
    Codex,
    Anthropic,
    Openai,
    Grok,
    Glm,
}

impl AcpAgentId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Grok => "grok",
            Self::Glm => "glm",
        }
    }
}

/// How the agent loop runs. `Acp` spawns an external ACP binary; `Anthropic`,
/// `Openai`, `Grok`, and `Glm` call their APIs directly in-process and are
/// gated on an API key instead of a PATH binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AcpProvider {
    Acp,
    ClaudeCli,
    Anthropic,
    Openai,
    Grok,
    Glm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpAgentDef {
    pub id: AcpAgentId,
    pub name: &'static str,
    /// Primary CLI binary to look for on PATH.
    pub command: &'static str,
    /// Drop-in forks tried if `command` isn't on PATH.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_bins: Option<&'static [&'static str]>,
    /// Argv passed to the binary at spawn time.
    pub args: &'static [&'static str],
    /// True if this binary speaks ACP and can drive our MCP tool loop.
    pub acp_supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<AcpProvider>,
    /// Short hint shown in the picker when the binary isn't found.
    pub install_hint: &'static str,
    /// Command that signs the user in, shown when the binary is installed but unauthenticated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_hint: Option<&'static str>,
    /// Single character used in the picker tile.
    pub icon: &'static str,
}

pub const ACP_AGENTS: &[AcpAgentDef] = &[
    AcpAgentDef {
        id: AcpAgentId::Anthropic,
        name: "Claude (API)",
        command: "",
        fallback_bins: None,
        args: &[],
        acp_supported: true,
        provider: Some(AcpProvider::Anthropic),
        install_hint: "Add an Anthropic API key in settings",
        login_hint: None,
        icon: "A",
    },
    AcpAgentDef {
        id: AcpAgentId::Openai,
        name: "OpenAI (API)",
        command: "",
        fallback_bins: None,
        args: &[],
        acp_supported: true,
        provider: Some(AcpProvider::Openai),
        install_hint: "Add an OpenAI API key in settings",
        login_hint: None,
        icon: "O",
    },
    AcpAgentDef {
        id: AcpAgentId::Grok,
        name: "Grok (API)",
        command: "",
        fallback_bins: None,
        args: &[],
        acp_supported: true,
        provider: Some(AcpProvider::Grok),
        install_hint: "Add an xAI API key in settings",
        login_hint: Some("grok login"),
        icon: "G",
    },
    AcpAgentDef {
        id: AcpAgentId::Glm,
        name: "GLM (API)",
        command: "",
        fallback_bins: None,
        args: &[],
        acp_supported: true,
        provider: Some(AcpProvider::Glm),
        install_hint: "Add a Z.ai API key in settings",
        login_hint: None,
        icon: "Z",
    },
    AcpAgentDef {
        id: AcpAgentId::ClaudeCode,
        name: "Claude Code",
        command: "claude",
        fallback_bins: None,
        args: &[],
        acp_supported: true,
        // Driven directly over stream-json (src/main/providers/claude-cli-provider.ts).
        provider: Some(AcpProvider::ClaudeCli),
        install_hint: "npm i -g @anthropic-ai/claude-code",
        login_hint: Some("claude login"),
        icon: "C",
    },
    AcpAgentDef {
        id: AcpAgentId::Codex,
        name: "Codex",
        command: "codex-acp",
        fallback_bins: None,
        args: &[],
        acp_supported: true,
        provider: None,
        install_hint: "npm i -g @agentclientprotocol/codex-acp",
        login_hint: Some("codex login"),
        icon: "X",
    },
];

pub const ACP_PERMISSION_TIMEOUT_MS: u64 = 60_000;

/// One selectable model in the unified picker. Each row carries
/// the agent it routes to, so choosing a model implicitly selects the provider.
///
/// For API providers (`anthropic`/`openai`/`grok`) the `model_id` is exact and
/// applied directly. For subscription ACP agents (`claude-code`/`codex`) the real model
/// ids are only known once a session spawns, so `model_id` here is a best-effort
/// default the picker reconciles against the live list by id-or-name match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub agent_id: AcpAgentId,
    pub model_id: &'static str,
    pub name: &'static str,
}

const fn model(agent_id: AcpAgentId, model_id: &'static str, name: &'static str) -> CatalogModel {
    CatalogModel {
        agent_id,
        model_id,
        name,
    }
}

pub const MODEL_CATALOG: &[CatalogModel] = &[
    // Claude Code — aliases the Claude Code CLI exposes (confirmed live via
    // [acp:newSession] with CLI 2.1.292: default/opus/fable/sonnet/haiku plus
    // version ids). `default` follows the user's own `claude` model setting.
    // Aliases are used instead of version ids so the labels don't drift; the
    // picker reconciles against the live list when the session offers one.
    model(AcpAgentId::ClaudeCode, "default", "Default"),
    model(AcpAgentId::ClaudeCode, "fable", "Fable"),
    model(AcpAgentId::ClaudeCode, "opus", "Opus"),
    model(AcpAgentId::ClaudeCode, "sonnet", "Sonnet"),
    model(AcpAgentId::ClaudeCode, "haiku", "Haiku"),
    // Codex — ids are reconciled against the live ACP list by name; exact ids
    // are confirmed on first spawn (see [acp:newSession] in the dev log).
    model(AcpAgentId::Codex, "gpt-5-codex", "GPT-5 Codex"),
    model(AcpAgentId::Codex, "gpt-5", "GPT-5"),
    // Claude API — exact ids of the current generation.
    model(AcpAgentId::Anthropic, "claude-opus-5-5", "Claude Opus 5.5"),
    model(
        AcpAgentId::Anthropic,
        "claude-sonnet-5-5",
        "Claude Sonnet 5.5",
    ),
    model(
        AcpAgentId::Anthropic,
        "claude-haiku-5-5",
        "Claude Haiku 5.5",
    ),
    model(
        AcpAgentId::Anthropic,
        "claude-fable-5-2",
        "Claude Fable 5.2",
    ),
    // OpenAI API — exact ids (mirror of OPENAI_MODELS in the main process).
    model(AcpAgentId::Openai, "gpt-4o", "GPT-4o"),
    model(AcpAgentId::Openai, "gpt-4o-mini", "GPT-4o mini"),
    model(AcpAgentId::Openai, "o3", "o3"),
    model(AcpAgentId::Openai, "o4-mini", "o4-mini"),
    // Grok — exact ids (mirror of GROK_MODELS). Subscription login fills these in.
    model(AcpAgentId::Grok, "grok-4.7", "Grok 4.7"),
    model(AcpAgentId::Grok, "grok-4.6", "Grok 4.6"),
    model(AcpAgentId::Grok, "grok-4.5", "Grok 4.5"),
    model(AcpAgentId::Grok, "grok-4.3", "Grok 4.3"),
    // GLM (Z.ai) — exact ids (mirror of GLM_MODELS in the main process).
    model(AcpAgentId::Glm, "glm-5.3", "GLM-5.3"),
    model(AcpAgentId::Glm, "glm-5.3-flash", "GLM-5.3 Flash"),
    model(AcpAgentId::Glm, "glm-5.3-flashx", "GLM-5.3 FlashX"),
    model(AcpAgentId::Glm, "glm-5.2", "GLM-5.2"),
    model(AcpAgentId::Glm, "glm-4.6", "GLM-4.6"),
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::*;

    #[test]
    fn 에이전트_id는_타입스크립트와_같은_문자열로_직렬화된다() {
        let ids: Vec<&str> = ACP_AGENTS.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            ["anthropic", "openai", "grok", "glm", "claude-code", "codex"]
        );
        for agent in ACP_AGENTS {
            assert_eq!(json!(agent.id), json!(agent.id.as_str()));
            let parsed: AcpAgentId = serde_json::from_value(json!(agent.id.as_str())).unwrap();
            assert_eq!(parsed, agent.id);
        }
        assert!(serde_json::from_value::<AcpAgentId>(json!("gemini")).is_err());
    }

    #[test]
    fn 에이전트_정의는_camelcase로_직렬화되고_없는_필드는_빠진다() {
        let claude = ACP_AGENTS
            .iter()
            .find(|a| a.id == AcpAgentId::ClaudeCode)
            .unwrap();
        assert_eq!(
            json!(claude),
            json!({
                "id": "claude-code",
                "name": "Claude Code",
                "command": "claude",
                "args": [],
                "acpSupported": true,
                "provider": "claude-cli",
                "installHint": "npm i -g @anthropic-ai/claude-code",
                "loginHint": "claude login",
                "icon": "C"
            })
        );
        // Codex has no `provider` and the API agents no `loginHint`.
        let codex = json!(
            ACP_AGENTS
                .iter()
                .find(|a| a.id == AcpAgentId::Codex)
                .unwrap()
        );
        assert!(codex.get("provider").is_none());
        assert_eq!(codex["command"], "codex-acp");
        let anthropic = json!(ACP_AGENTS.first().unwrap());
        assert!(anthropic.get("loginHint").is_none());
        assert_eq!(anthropic["provider"], "anthropic");
    }

    #[test]
    fn 권한_요청_타임아웃은_60초다() {
        assert_eq!(ACP_PERMISSION_TIMEOUT_MS, 60_000);
    }

    #[test]
    fn 모델_카탈로그는_에이전트별_모델_수와_순서를_유지한다() {
        let count = |id| MODEL_CATALOG.iter().filter(|m| m.agent_id == id).count();
        assert_eq!(MODEL_CATALOG.len(), 24);
        assert_eq!(count(AcpAgentId::ClaudeCode), 5);
        assert_eq!(count(AcpAgentId::Codex), 2);
        assert_eq!(count(AcpAgentId::Anthropic), 4);
        assert_eq!(count(AcpAgentId::Openai), 4);
        assert_eq!(count(AcpAgentId::Grok), 4);
        assert_eq!(count(AcpAgentId::Glm), 5);
        assert_eq!(
            json!(MODEL_CATALOG.first().unwrap()),
            json!({ "agentId": "claude-code", "modelId": "default", "name": "Default" })
        );
        assert_eq!(MODEL_CATALOG.last().unwrap().model_id, "glm-4.6");
    }

    #[test]
    fn 두_표_전체가_타입스크립트의_json과_글자까지_같다() {
        // JSON.stringify(ACP_AGENTS) and JSON.stringify(MODEL_CATALOG) from
        // constants.ts: one wrong id, name or field order anywhere fails this.
        assert_eq!(
            serde_json::to_string(ACP_AGENTS).unwrap(),
            concat!(
                r#"[{"id":"anthropic","name":"Claude (API)","command":"","args":[],"acpSupported":true,"provider":"anthropic","installHint":"Add an Anthropic API key in settings","icon":"A"},"#,
                r#"{"id":"openai","name":"OpenAI (API)","command":"","args":[],"acpSupported":true,"provider":"openai","installHint":"Add an OpenAI API key in settings","icon":"O"},"#,
                r#"{"id":"grok","name":"Grok (API)","command":"","args":[],"acpSupported":true,"provider":"grok","installHint":"Add an xAI API key in settings","loginHint":"grok login","icon":"G"},"#,
                r#"{"id":"glm","name":"GLM (API)","command":"","args":[],"acpSupported":true,"provider":"glm","installHint":"Add a Z.ai API key in settings","icon":"Z"},"#,
                r#"{"id":"claude-code","name":"Claude Code","command":"claude","args":[],"acpSupported":true,"provider":"claude-cli","installHint":"npm i -g @anthropic-ai/claude-code","loginHint":"claude login","icon":"C"},"#,
                r#"{"id":"codex","name":"Codex","command":"codex-acp","args":[],"acpSupported":true,"installHint":"npm i -g @agentclientprotocol/codex-acp","loginHint":"codex login","icon":"X"}]"#,
            )
        );
        assert_eq!(
            serde_json::to_string(MODEL_CATALOG).unwrap(),
            concat!(
                r#"[{"agentId":"claude-code","modelId":"default","name":"Default"},{"agentId":"claude-code","modelId":"fable","name":"Fable"},"#,
                r#"{"agentId":"claude-code","modelId":"opus","name":"Opus"},{"agentId":"claude-code","modelId":"sonnet","name":"Sonnet"},"#,
                r#"{"agentId":"claude-code","modelId":"haiku","name":"Haiku"},{"agentId":"codex","modelId":"gpt-5-codex","name":"GPT-5 Codex"},"#,
                r#"{"agentId":"codex","modelId":"gpt-5","name":"GPT-5"},{"agentId":"anthropic","modelId":"claude-opus-5-5","name":"Claude Opus 5.5"},"#,
                r#"{"agentId":"anthropic","modelId":"claude-sonnet-5-5","name":"Claude Sonnet 5.5"},{"agentId":"anthropic","modelId":"claude-haiku-5-5","name":"Claude Haiku 5.5"},"#,
                r#"{"agentId":"anthropic","modelId":"claude-fable-5-2","name":"Claude Fable 5.2"},{"agentId":"openai","modelId":"gpt-4o","name":"GPT-4o"},"#,
                r#"{"agentId":"openai","modelId":"gpt-4o-mini","name":"GPT-4o mini"},{"agentId":"openai","modelId":"o3","name":"o3"},"#,
                r#"{"agentId":"openai","modelId":"o4-mini","name":"o4-mini"},{"agentId":"grok","modelId":"grok-4.7","name":"Grok 4.7"},"#,
                r#"{"agentId":"grok","modelId":"grok-4.6","name":"Grok 4.6"},{"agentId":"grok","modelId":"grok-4.5","name":"Grok 4.5"},"#,
                r#"{"agentId":"grok","modelId":"grok-4.3","name":"Grok 4.3"},{"agentId":"glm","modelId":"glm-5.3","name":"GLM-5.3"},"#,
                r#"{"agentId":"glm","modelId":"glm-5.3-flash","name":"GLM-5.3 Flash"},{"agentId":"glm","modelId":"glm-5.3-flashx","name":"GLM-5.3 FlashX"},"#,
                r#"{"agentId":"glm","modelId":"glm-5.2","name":"GLM-5.2"},{"agentId":"glm","modelId":"glm-4.6","name":"GLM-4.6"}]"#,
            )
        );
    }

    #[test]
    fn 카탈로그의_모든_모델은_정의된_에이전트를_가리키고_중복이_없다() {
        let mut seen = HashSet::new();
        for m in MODEL_CATALOG {
            assert!(ACP_AGENTS.iter().any(|a| a.id == m.agent_id));
            assert!(seen.insert((m.agent_id, m.model_id)), "{}", m.model_id);
        }
    }
}
