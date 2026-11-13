//! Partial port of `src/main/agent-probe.ts` — the renderable health of each agent.
//!
//! What is ported here is the self-contained surface: the [`AgentStatus`] /
//! [`AgentHealth`] / [`AgentProbeDef`] shapes and [`failure_detail`], the helper
//! that folds a failure reason, exit code and a short stderr tail into one line.
//!
//! Not ported: `probeAgents` and the per-provider probes (`probeAnthropic`,
//! `probeOpenAi`, `probeGrok`, `probeGlm`, `probeAcp`, `probeClaudeCli`, and the
//! ACP `handshake`). Each needs a provider adapter — the Anthropic/OpenAI SDK
//! clients, `settings::get_api_key`, `grok-login`, `acp-session` — that has not
//! been ported yet, so this file deliberately carries no `Port of` marker until
//! those land.

use serde::{Deserialize, Serialize};

/// How ready an agent is to be used, as the setup UI renders it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentStatus {
    Ready,
    NotInstalled,
    NeedsKey,
    NeedsLogin,
    AuthFailed,
    Failed,
}

/// One agent to probe. `provider` defaults to `"acp"` in the TypeScript; here it
/// is an `Option` and the caller treats `None` the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProbeDef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_bins: Option<Vec<String>>,
}

/// The probe result for one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHealth {
    pub id: String,
    pub status: AgentStatus,
    /// Absolute path of the ACP binary, or `None` for API providers.
    pub resolved_path: Option<String>,
    /// Short renderable reason. Never contains a token or key.
    pub detail: Option<String>,
    /// True when an existing CLI login covers this agent — no setup needed.
    pub auto: bool,
    /// Plan the existing login advertises (`"max"`, `"chatgpt"`), or `None`.
    pub plan: Option<String>,
}

/// The stderr tail is cut to this many characters so a stack trace cannot blow up
/// the modal. The TypeScript cuts to 200 UTF-16 units; this counts characters,
/// which agrees for the ASCII diagnostics a launch failure prints.
const STDERR_TAIL_MAX: usize = 200;

/// Fold the raw failure reason, exit code and a short stderr tail into one
/// renderable line. Keeps only the last couple of stderr lines so a stack trace
/// can't blow up the modal, and never emits a token (initialize doesn't return
/// one, and a failed launch only prints its own diagnostics).
pub fn failure_detail(base: &str, exit_info: Option<&str>, stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .split('\n')
        .map(|line| line.trim_end_matches('\r').trim())
        .filter(|line| !line.is_empty())
        .collect();
    let tail_lines = &lines[lines.len().saturating_sub(2)..];
    let joined = tail_lines.join(" ");
    let tail: String = joined.chars().take(STDERR_TAIL_MAX).collect();

    [Some(base), exit_info, Some(tail.as_str())]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" — ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 원인이_없으면_기본_메시지만_반환한다() {
        assert_eq!(
            failure_detail("ACP connection closed", None, ""),
            "ACP connection closed"
        );
    }

    #[test]
    fn 종료_코드와_stderr_꼬리를_붙여_실제_원인을_드러낸다() {
        let stderr = "'node' is not recognized as an internal or external command,\n\
             operable program or batch file.\n";
        assert_eq!(
            failure_detail("ACP connection closed", Some("exit 1"), stderr),
            "ACP connection closed — exit 1 — 'node' is not recognized as an internal or \
             external command, operable program or batch file."
        );
    }

    #[test]
    fn stderr는_마지막_두_줄만_200자로_제한한다() {
        let stderr = ["line one", "line two", "line three", "line four"].join("\n");
        assert_eq!(
            failure_detail("boom", None, &stderr),
            "boom — line three line four"
        );

        let long = "x".repeat(500);
        assert!(failure_detail("boom", None, &long).len() <= "boom — ".len() + STDERR_TAIL_MAX);
    }

    #[test]
    fn 빈_값은_구분자_없이_걸러낸다() {
        assert_eq!(failure_detail("boom", None, "   \n  \n"), "boom");
        assert_eq!(failure_detail("boom", Some("exit 2"), ""), "boom — exit 2");
    }
}
