//! Port of `src/main/providers/openai-tool-cap.ts` — the 128-tool backstop for Chat Completions.
//!
//! OpenAI's Chat Completions API rejects requests carrying more than 128 tools
//! (Anthropic has no such limit). Profile filtering runs first. This cap is the
//! backstop after that filter: drop the nine bp_* names (hidden from every
//! in-app profile), then non-core names from the tail. Core stays even if the
//! list is still over the cap. The caller logs a warning, not an error.

use serde::Serialize;

use crate::tool_profile::{BucketSpec, tool_bucket};

pub const OPENAI_MAX_TOOLS: usize = 128;

// Ordered "drop first" list. Only as many entries as needed are removed.
pub const OPENAI_DROP_ORDER: [&str; 12] = [
    // Native dialogs made the page-side override re-inject a rarely needed fallback.
    "dialog_inject_override",
    "dialog_clear_history",
    // A context saver, but list_requests covers it; cutting it keeps crypto_trace_list from the tail cut.
    "list_endpoints",
    "bp_add",
    "bp_list",
    "bp_remove",
    "bp_status",
    "bp_resume",
    "bp_step_over",
    "bp_step_into",
    "bp_step_out",
    "bp_eval_in_frame",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CapResult<T> {
    pub tools: Vec<T>,
    /// Dropped names, each once, in the order they were dropped.
    pub dropped: Vec<String>,
}

// Trim `tools` to at most OPENAI_MAX_TOOLS, removing OPENAI_DROP_ORDER entries
// first (in order) and, only if that isn't enough, non-core names from the tail.
// `name` reads a tool's `function.name`.
pub fn cap_tools_for_openai<T>(
    tools: Vec<T>,
    name: impl for<'a> Fn(&'a T) -> &'a str,
) -> CapResult<T> {
    if tools.len() <= OPENAI_MAX_TOOLS {
        return CapResult {
            tools,
            dropped: Vec::new(),
        };
    }

    let mut dropped: Vec<String> = Vec::new();
    for candidate in OPENAI_DROP_ORDER {
        // The list is longer than the cap here, so this cannot underflow.
        if tools.len() - dropped.len() <= OPENAI_MAX_TOOLS {
            break;
        }
        if tools.iter().any(|t| name(t) == candidate) {
            dropped.push(candidate.to_owned());
        }
    }

    let mut kept: Vec<T> = tools
        .into_iter()
        .filter(|t| !dropped.iter().any(|d| d == name(t)))
        .collect();
    if kept.len() > OPENAI_MAX_TOOLS {
        // Denylist exhausted. Drop non-core names from the end until the list fits.
        // A core tool registered last is skipped, not cut.
        let mut overflow = kept.len() - OPENAI_MAX_TOOLS;
        let mut cut_at = vec![false; kept.len()];
        for (tool, cut) in kept.iter().zip(cut_at.iter_mut()).rev() {
            if overflow == 0 {
                break;
            }
            if tool_bucket(name(tool)) == Some(BucketSpec::Core) {
                continue;
            }
            *cut = true;
            overflow -= 1;
        }
        let mut next: Vec<T> = Vec::with_capacity(OPENAI_MAX_TOOLS);
        for (tool, cut) in kept.into_iter().zip(cut_at) {
            if !cut {
                next.push(tool);
                continue;
            }
            let cut_name = name(&tool);
            if !dropped.iter().any(|d| d == cut_name) {
                dropped.push(cut_name.to_owned());
            }
        }
        kept = next;
    }

    CapResult {
        tools: kept,
        dropped,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::path::Path;


    use super::*;
    use crate::tool_profile::{ToolProfile, advertised_names, is_advertised};

    fn cap(names: &[String]) -> CapResult<String> {
        cap_tools_for_openai(names.to_vec(), |name| name.as_str())
    }

    fn numbered(prefix: &str, count: usize) -> Vec<String> {
        (0..count).map(|i| format!("{prefix}{i}")).collect()
    }

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn 한도_이하의_목록은_그대로_둔다() {
        let r = cap(&numbered("t", OPENAI_MAX_TOOLS));
        assert_eq!(r.tools.len(), OPENAI_MAX_TOOLS);
        assert_eq!(r.dropped, Vec::<String>::new());
    }

    #[test]
    fn 거부_목록을_순서대로_필요한_만큼만_먼저_버린다_141개에서_128개() {
        let mut names = owned(&OPENAI_DROP_ORDER);
        names.extend(numbered("keep", 129));
        assert_eq!(names.len(), 141);
        let r = cap(&names);
        assert_eq!(r.tools.len(), 128);
        let mut expected = owned(&OPENAI_DROP_ORDER);
        expected.push("keep128".to_owned());
        assert_eq!(r.dropped, expected);
        assert!(r.tools.iter().all(|t| !r.dropped.contains(t)));
    }

    #[test]
    fn 목록에_없는_거부_이름은_무시한다() {
        // 129 tools, only one denylist name present -> that one drops, list fits.
        let mut names = owned(&["bp_add"]);
        names.extend(numbered("keep", 128));
        let r = cap(&names);
        assert_eq!(r.tools.len(), 128);
        assert_eq!(r.dropped, ["bp_add"]);
    }

    #[test]
    fn 거부_목록이_바닥나면_꼬리에서_자른다() {
        let r = cap(&numbered("x", 130));
        assert_eq!(r.tools.len(), 128);
        assert_eq!(r.dropped, ["x128", "x129"]);
    }

    #[test]
    fn 꼬리의_core_도구는_남기고_그_앞의_비core를_자른다() {
        let mut names = numbered("x", 128);
        names.push("browser_snapshot".to_owned());
        let r = cap(&names);
        assert_eq!(r.tools.len(), 128);
        assert_eq!(r.dropped, ["x127"]);
        assert!(r.tools.iter().any(|t| t == "browser_snapshot"));
    }

    #[test]
    fn reverse에_browse_전용_34개를_더한_152개에서_list_endpoints와_등록_순_꼬리를_자른다() {
        let order = registration_order();
        assert_eq!(order.len(), 177);
        let browse_only: Vec<&str> = advertised_names(ToolProfile::Browse, [])
            .into_iter()
            .filter(|name| !is_advertised(name, ToolProfile::Reverse))
            .collect();
        assert_eq!(browse_only.len(), 34);
        let allowed: HashSet<&str> = advertised_names(ToolProfile::Reverse, [])
            .into_iter()
            .chain(browse_only)
            .collect();
        assert_eq!(allowed.len(), 152);
        let filtered: Vec<String> = order
            .into_iter()
            .filter(|name| allowed.contains(name.as_str()))
            .collect();
        assert_eq!(filtered.len(), 152);
        assert_eq!(
            filtered.last().map(String::as_str),
            Some("tool_profile_set")
        );
        let r = cap(&filtered);
        assert_eq!(r.tools.len(), 128);
        assert_eq!(
            r.dropped,
            [
                "dialog_inject_override",
                "dialog_clear_history",
                "list_endpoints",
                "delete_macro",
                "scan_secrets",
                "crypto_trace_start",
                "crypto_trace_list",
                "crypto_trace_stop",
                "fetch_body",
                "detect_antibot_vm",
                "graphql_introspect",
                "protobuf_decode",
                "list_wasm",
                "wasm_decompile",
                "wasm_info",
                "grep_wasm",
                "wasm_xref",
                "cli_arm",
                "cli_compile",
                "cli_list",
                "cli_describe",
                "cli_run",
                "cli_writes",
                "cli_export"
            ]
        );
        for name in ["browser_snapshot", "tool_profile_set", "finding_export"] {
            assert!(r.tools.iter().any(|t| t == name), "{name}");
        }
    }

    /// Tool names in the order the TypeScript server registered them — frozen
    /// from history (snapshot/ts-tool-order.json); the app itself is gone.
    fn registration_order() -> Vec<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("snapshot")
            .join("ts-tool-order.json");
        fs::read_to_string(path)
            .and_then(|text| Ok(serde_json::from_str(&text)?))
            .unwrap()
    }

    // --- Not in the TypeScript tests. ---

    #[test]
    fn core만으로_한도를_넘으면_아무것도_자르지_않는다() {
        let core: Vec<String> = (0..130).map(|_| "browser_snapshot".to_owned()).collect();
        let r = cap(&core);
        assert_eq!(r.tools.len(), 130);
        assert_eq!(r.dropped, Vec::<String>::new());
    }

    #[test]
    fn 같은_이름이_여러_번_잘려도_dropped에는_한_번만_적는다() {
        let mut names = numbered("x", 127);
        names.extend(owned(&["dup", "dup", "dup"]));
        let r = cap(&names);
        assert_eq!(r.tools.len(), 128);
        assert_eq!(r.dropped, ["dup"]);
        // One "dup" survives: only the overflow is cut from the tail.
        assert_eq!(r.tools.iter().filter(|t| *t == "dup").count(), 1);
    }

    #[test]
    fn 결과는_tools와_dropped_필드로_직렬화된다() {
        let r = cap(&owned(&["a"]));
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::json!({ "tools": ["a"], "dropped": [] })
        );
    }
}
