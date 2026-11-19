//! Port of `src/renderer/src/ai/session-grants.ts` — "allow all <tool> calls this session" grants.
//!
//! Grants for gated tools are kept here instead of answering allow_always: that
//! would hand the decision to claude-agent-acp, which then stops asking — and
//! stops the audit trail. Keyed by ACP session, so a new chat starts with no
//! grants. In-memory only: an app restart clears them too.
//!
//! The TypeScript kept the set as a module variable and imported `gatedToolName`
//! from `src/shared/risky-tools.ts`. Here the set is a struct, and since that
//! module is ported in another crate, the gated-tool decision comes in through
//! the [`RiskyTools`] trait.

use std::collections::HashSet;

use serde_json::Value;

use crate::RiskyTools;

#[derive(Clone, Debug, Default)]
pub struct SessionGrants {
    grants: HashSet<String>,
}

fn grant_key(req: &Value, risky: &impl RiskyTools) -> Option<String> {
    let tool = risky.gated_tool_name(req).filter(|tool| !tool.is_empty())?;
    let session_id = req
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(format!("{session_id}:{tool}"))
}

impl SessionGrants {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn grant_session(&mut self, req: &Value, risky: &impl RiskyTools) {
        if let Some(key) = grant_key(req, risky) {
            self.grants.insert(key);
        }
    }

    pub fn has_session_grant(&self, req: &Value, risky: &impl RiskyTools) -> bool {
        grant_key(req, risky).is_some_and(|key| self.grants.contains(&key))
    }

    pub fn clear_session_grants(&mut self) {
        self.grants.clear();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_risky_tools::TsRiskyTools;

    fn req(session_id: &str, title: &str, raw_input: Option<Value>) -> Value {
        let mut tool_call = json!({ "toolCallId": "t", "title": title });
        if let Some(raw_input) = raw_input {
            tool_call["rawInput"] = raw_input;
        }
        json!({ "sessionId": session_id, "options": [], "toolCall": tool_call })
    }

    // The TypeScript suite cleared the module state with `afterEach`; each test
    // here owns a fresh SessionGrants instead.

    #[test]
    fn 허용한_툴은_같은_세션의_다음_호출에서_grant가_있다() {
        let mut grants = SessionGrants::new();
        grants.grant_session(
            &req(
                "s1",
                "mcp__rever-traffic__repeater_send",
                Some(json!({ "modifications": { "method": "POST" } })),
            ),
            &TsRiskyTools,
        );
        assert!(grants.has_session_grant(
            &req(
                "s1",
                "mcp__rever-traffic__repeater_send",
                Some(json!({ "modifications": { "method": "DELETE" } })),
            ),
            &TsRiskyTools,
        ));
    }

    #[test]
    fn 다른_세션이나_다른_툴에는_번지지_않는다() {
        let mut grants = SessionGrants::new();
        grants.grant_session(
            &req("s1", "mcp__rever-traffic__repeater_send", None),
            &TsRiskyTools,
        );
        assert!(!grants.has_session_grant(
            &req("s2", "mcp__rever-traffic__repeater_send", None),
            &TsRiskyTools
        ));
        assert!(!grants.has_session_grant(
            &req("s1", "mcp__rever-traffic__intruder_run", None),
            &TsRiskyTools
        ));
    }

    #[test]
    fn 게이트_대상이_아닌_툴은_grant를_저장하지_않는다() {
        let mut grants = SessionGrants::new();
        grants.grant_session(
            &req("s1", "mcp__rever-traffic__list_requests", None),
            &TsRiskyTools,
        );
        assert!(!grants.has_session_grant(
            &req("s1", "mcp__rever-traffic__list_requests", None),
            &TsRiskyTools
        ));
        assert!(grants.grants.is_empty());
    }

    #[test]
    fn clear_session_grants로_모두_초기화된다() {
        let mut grants = SessionGrants::new();
        grants.grant_session(
            &req("s1", "mcp__rever-traffic__burst_send", None),
            &TsRiskyTools,
        );
        grants.clear_session_grants();
        assert!(!grants.has_session_grant(
            &req("s1", "mcp__rever-traffic__burst_send", None),
            &TsRiskyTools
        ));
    }
}
