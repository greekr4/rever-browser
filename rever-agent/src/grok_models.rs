//! Port of `src/main/providers/grok-models.ts` — curated xAI chat models for the in-app
//! agent. Image, video, and voice models are not chat-agent models and stay out of this list.
//! Ids: https://docs.x.ai/docs/models — Grok 4.7 is the code and chat flagship.
//! Chat Completions at https://api.x.ai/v1 is the wire format this app already
//! runs for OpenAI, including the 128-tool cap.

use serde::Serialize;

pub const XAI_BASE_URL: &str = "https://api.x.ai/v1";

/// Official JS sample uses 360s so a reasoning completion is not cut off.
pub const GROK_CHAT_TIMEOUT_MS: u64 = 360_000;

pub const GROK_SESSION_PREFIX: &str = "grok:";

pub const GROK_DEFAULT_MODEL: &str = "grok-4.7";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokModelInfo {
    pub model_id: &'static str,
    pub name: &'static str,
    pub description: Option<&'static str>,
}

const fn model(
    model_id: &'static str,
    name: &'static str,
    description: &'static str,
) -> GrokModelInfo {
    GrokModelInfo {
        model_id,
        name,
        description: Some(description),
    }
}

pub const GROK_MODELS: &[GrokModelInfo] = &[
    model("grok-4.7", "Grok 4.7", "Flagship for code and chat"),
    model("grok-4.6", "Grok 4.6", "500k context"),
    model("grok-4.5", "Grok 4.5", "500k context"),
    model("grok-4.3", "Grok 4.3", "1M context"),
];

pub fn is_grok_session_id(session_id: &str) -> bool {
    session_id.starts_with(GROK_SESSION_PREFIX)
}

// Not ported from grok-models.test.ts: the two cases that compare this list with
// MODEL_CATALOG / ACP_AGENTS (renderer constants.ts) and AGENT_CATALOG
// (agent-catalog.ts). Those catalogs have no Rust port to compare against yet.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 기본_모델은_grok_4_7이고_채팅_모델만_담는다() {
        assert_eq!(GROK_DEFAULT_MODEL, "grok-4.7");
        let ids: Vec<&str> = GROK_MODELS.iter().map(|m| m.model_id).collect();
        assert_eq!(ids, ["grok-4.7", "grok-4.6", "grok-4.5", "grok-4.3"]);
        assert!(GROK_MODELS.iter().all(|m| {
            let id = m.model_id.to_lowercase();
            !["imagine", "voice", "video"]
                .iter()
                .any(|kind| id.contains(kind))
        }));
        assert_eq!(XAI_BASE_URL, "https://api.x.ai/v1");
    }

    #[test]
    fn grok_세션_id는_openai_세션_id와_겹치지_않는다() {
        assert!(is_grok_session_id("grok:abc"));
        assert!(!is_grok_session_id("openai:abc"));
        assert!(!is_grok_session_id("anthropic:abc"));
    }
}
