//! Port of `src/main/providers/direct-tool-gate.ts` — per-turn tool gate for the direct API providers.

use std::collections::HashSet;

use crate::tool_profile::{ToolProfile, advertised_names};

pub fn direct_tool_refusal(name: &str) -> String {
    format!("refused {name}: not in the current tool profile or this session's prior calls.")
}

/// Names this completion may call. `dispatched` is only names already passed to `mcp.callTool`.
pub fn begin_direct_turn(profile: ToolProfile, dispatched: &HashSet<String>) -> HashSet<String> {
    advertised_names(profile, dispatched.iter().map(String::as_str))
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// Refuse a name outside `allowed` without recording it: `Err` carries the
/// refusal text to hand back as the tool result.
/// A name that will be passed to `mcp.callTool` is added first, including when that call fails.
pub fn take_direct_tool(
    name: &str,
    allowed: &HashSet<String>,
    dispatched: &mut HashSet<String>,
) -> Result<(), String> {
    if !allowed.contains(name) {
        return Err(direct_tool_refusal(name));
    }
    dispatched.insert(name.to_owned());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse에서_빈_dispatched로_intruder_run을_두_번_거부하고_다음_목록에_넣지_않는다() {
        let mut dispatched = HashSet::new();
        let allowed = begin_direct_turn(ToolProfile::Browse, &dispatched);
        let first = take_direct_tool("intruder_run", &allowed, &mut dispatched);
        assert_eq!(first, Err(direct_tool_refusal("intruder_run")));
        assert!(dispatched.is_empty());

        let next = begin_direct_turn(ToolProfile::Browse, &dispatched);
        assert!(!next.contains("intruder_run"));
        let second = take_direct_tool("intruder_run", &next, &mut dispatched);
        assert_eq!(second, Err(direct_tool_refusal("intruder_run")));
        assert!(dispatched.is_empty());
    }

    #[test]
    fn call_tool에_넘긴_browser_snapshot은_다음_목록에_남는다() {
        let mut dispatched = HashSet::new();
        let allowed = begin_direct_turn(ToolProfile::Browse, &dispatched);
        assert_eq!(
            take_direct_tool("browser_snapshot", &allowed, &mut dispatched),
            Ok(())
        );
        assert!(dispatched.contains("browser_snapshot"));
        assert!(begin_direct_turn(ToolProfile::Browse, &dispatched).contains("browser_snapshot"));
    }

    #[test]
    fn 같은_배치의_allowed는_tool_profile_set_이후에도_바뀌지_않는다() {
        let mut dispatched = HashSet::new();
        let allowed = begin_direct_turn(ToolProfile::Browse, &dispatched);
        assert_eq!(
            take_direct_tool("tool_profile_set", &allowed, &mut dispatched),
            Ok(())
        );
        let intruder = take_direct_tool("intruder_run", &allowed, &mut dispatched);
        assert!(intruder.is_err());
        assert_eq!(dispatched.iter().collect::<Vec<_>>(), ["tool_profile_set"]);
        assert!(!begin_direct_turn(ToolProfile::Browse, &dispatched).contains("intruder_run"));
    }

    // --- Not in the TypeScript tests. ---

    #[test]
    fn 거부_문구는_도구_이름만_바뀐다() {
        assert_eq!(
            direct_tool_refusal("intruder_run"),
            "refused intruder_run: not in the current tool profile or this session's prior calls."
        );
    }

    #[test]
    fn 다른_프로파일에서_이미_넘긴_이름은_허용하고_hidden과_모르는_이름은_허용하지_않는다() {
        let mut dispatched: HashSet<String> = ["grep_scripts", "bp_add", "not_a_tool"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let allowed = begin_direct_turn(ToolProfile::Browse, &dispatched);
        assert_eq!(allowed.len(), 85);
        assert_eq!(
            take_direct_tool("grep_scripts", &allowed, &mut dispatched),
            Ok(())
        );
        assert!(take_direct_tool("bp_add", &allowed, &mut dispatched).is_err());
        assert!(take_direct_tool("not_a_tool", &allowed, &mut dispatched).is_err());
        assert!(take_direct_tool("", &allowed, &mut dispatched).is_err());
    }
}
