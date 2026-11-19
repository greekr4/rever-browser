//! Port of `src/main/agent-catalog.ts` — the only agents the main process will ever spawn or probe.
//!
//! The renderer refers to them by id; it never sends a command line over IPC,
//! so a compromised renderer cannot turn `acp:spawn` into arbitrary code execution.
//! Keep in sync with ACP_AGENTS in src/renderer/src/constants.ts (UI metadata
//! such as names, icons and install hints lives there).

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentProvider {
    Acp,
    ClaudeCli,
    Anthropic,
    Openai,
    Grok,
    Glm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogAgent {
    pub id: &'static str,
    pub provider: AgentProvider,
    /// Primary CLI binary to look for on PATH ('' for in-process API providers).
    pub command: &'static str,
    /// Drop-in forks tried if `command` isn't on PATH.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_bins: Option<&'static [&'static str]>,
    /// Argv passed to the binary at spawn time.
    pub args: &'static [&'static str],
}

const fn agent(id: &'static str, provider: AgentProvider, command: &'static str) -> CatalogAgent {
    CatalogAgent {
        id,
        provider,
        command,
        fallback_bins: None,
        args: &[],
    }
}

pub const AGENT_CATALOG: &[CatalogAgent] = &[
    agent("anthropic", AgentProvider::Anthropic, ""),
    agent("openai", AgentProvider::Openai, ""),
    agent("grok", AgentProvider::Grok, ""),
    agent("glm", AgentProvider::Glm, ""),
    // The user's `claude` CLI, driven over stream-json (resolved by findClaudeCli).
    agent("claude-code", AgentProvider::ClaudeCli, "claude"),
    agent("codex", AgentProvider::Acp, "codex-acp"),
];

pub fn catalog_agent(id: &str) -> Option<&'static CatalogAgent> {
    AGENT_CATALOG.iter().find(|a| a.id == id)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::*;

    #[test]
    fn 카탈로그는_여섯_에이전트를_타입스크립트와_같은_순서와_json으로_싣는다() {
        assert_eq!(
            serde_json::to_value(AGENT_CATALOG).unwrap(),
            json!([
                { "id": "anthropic", "provider": "anthropic", "command": "", "args": [] },
                { "id": "openai", "provider": "openai", "command": "", "args": [] },
                { "id": "grok", "provider": "grok", "command": "", "args": [] },
                { "id": "glm", "provider": "glm", "command": "", "args": [] },
                { "id": "claude-code", "provider": "claude-cli", "command": "claude", "args": [] },
                { "id": "codex", "provider": "acp", "command": "codex-acp", "args": [] }
            ])
        );
    }

    #[test]
    fn id로_찾고_모르는_id와_대소문자가_다른_id는_none이다() {
        assert_eq!(
            catalog_agent("claude-code").map(|a| (a.provider, a.command)),
            Some((AgentProvider::ClaudeCli, "claude"))
        );
        assert_eq!(
            catalog_agent("codex").map(|a| (a.provider, a.command)),
            Some((AgentProvider::Acp, "codex-acp"))
        );
        assert_eq!(catalog_agent("gemini"), None);
        assert_eq!(catalog_agent("Codex"), None);
        assert_eq!(catalog_agent(""), None);
        // A command line is never an id.
        assert_eq!(catalog_agent("codex-acp"), None);
        assert_eq!(catalog_agent("claude"), None);
    }

    #[test]
    fn id는_겹치지_않고_api_제공자만_명령이_비어_있다() {
        let ids: HashSet<&str> = AGENT_CATALOG.iter().map(|a| a.id).collect();
        assert_eq!(ids.len(), AGENT_CATALOG.len());
        for a in AGENT_CATALOG {
            let spawned = matches!(a.provider, AgentProvider::Acp | AgentProvider::ClaudeCli);
            assert_eq!(!a.command.is_empty(), spawned, "{}", a.id);
        }
    }

    #[test]
    fn fallback_bins는_있을_때만_camel_case로_직렬화된다() {
        let with_forks = CatalogAgent {
            fallback_bins: Some(&["fork-a", "fork-b"]),
            ..agent("x", AgentProvider::Acp, "x-acp")
        };
        assert_eq!(
            serde_json::to_value(with_forks).unwrap(),
            json!({
                "id": "x",
                "provider": "acp",
                "command": "x-acp",
                "fallbackBins": ["fork-a", "fork-b"],
                "args": []
            })
        );
    }
}
