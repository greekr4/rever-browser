//! Port of `src/main/mcp/tool-profile.ts` — pure tool-profile registry. No Electron, no I/O.
//!
//! Which names a turn advertises is decided here. Registration stays complete
//! for external clients; later PRs filter at advertise time.
//! See docs/design-mcp-tool-profiles.md.

use std::collections::HashSet;
use std::fmt;

use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::tool_profile_line::{ToolProfileId, tool_profile_turn_line};

/// The TypeScript declares this union twice (here and in tool-profile-line.ts);
/// one Rust enum serves both names.
pub type ToolProfile = ToolProfileId;

pub const TOOL_PROFILES: [ToolProfile; 3] = [
    ToolProfile::Browse,
    ToolProfile::Reverse,
    ToolProfile::Probe,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolBucket {
    Browse,
    Reverse,
    Probe,
    Hidden,
}

/// core: every in-app profile. hidden: none. Otherwise the union of the listed profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BucketSpec {
    Core,
    Of(&'static [ToolBucket]),
}

impl BucketSpec {
    pub fn is_hidden(self) -> bool {
        matches!(self, Self::Of(buckets) if buckets.contains(&ToolBucket::Hidden))
    }

    fn advertises(self, profile: ToolProfile) -> bool {
        let wanted = match profile {
            ToolProfile::Browse => ToolBucket::Browse,
            ToolProfile::Reverse => ToolBucket::Reverse,
            ToolProfile::Probe => ToolBucket::Probe,
        };
        match self {
            Self::Core => true,
            Self::Of(buckets) => !self.is_hidden() && buckets.contains(&wanted),
        }
    }
}

const CORE: BucketSpec = BucketSpec::Core;
const BROWSE: BucketSpec = BucketSpec::Of(&[ToolBucket::Browse]);
const BROWSE_REVERSE: BucketSpec = BucketSpec::Of(&[ToolBucket::Browse, ToolBucket::Reverse]);
const REVERSE: BucketSpec = BucketSpec::Of(&[ToolBucket::Reverse]);
const REVERSE_PROBE: BucketSpec = BucketSpec::Of(&[ToolBucket::Reverse, ToolBucket::Probe]);
const PROBE: BucketSpec = BucketSpec::Of(&[ToolBucket::Probe]);
const HIDDEN: BucketSpec = BucketSpec::Of(&[ToolBucket::Hidden]);

/// Registry in declaration order: `advertised_names` lists tools in this order.
pub const TOOL_BUCKETS: &[(&str, BucketSpec)] = &[
    // core (27) — existing 22 + tool_profile_set + dialog answer/history + downloads
    ("tool_profile_set", CORE),
    ("browser_navigate", CORE),
    ("browser_snapshot", CORE),
    ("browser_click", CORE),
    ("browser_type", CORE),
    ("browser_scroll", CORE),
    ("browser_press_key", CORE),
    ("browser_click_selector", CORE),
    ("browser_type_selector", CORE),
    ("browser_evaluate", CORE),
    ("dom_extract", CORE),
    ("browser_wait_for", CORE),
    ("list_requests", CORE),
    ("get_request", CORE),
    ("tool_result_more", CORE),
    ("console_logs", CORE),
    ("console_exceptions", CORE),
    ("scope_status", CORE),
    ("scope_set", CORE),
    ("scope_add", CORE),
    ("scope_clear", CORE),
    ("lab_target", CORE),
    ("finding_add", CORE),
    ("finding_list", CORE),
    // Export belongs with add/list: an agent that can record findings in any
    // profile must be able to write the report in that profile too — gating it to
    // reverse made finding_export look "missing" to a browse/probe session.
    ("finding_export", CORE),
    // The deterministic site-sweep escape hatch — must be visible in every
    // profile so an agent never hand-orchestrates per-tab sub-agents instead.
    ("redteam_start", CORE),
    // Pages pop dialogs and downloads in any profile; the agent must be able to steer them.
    ("browser_handle_dialog", CORE),
    ("dialog_history", CORE),
    ("list_downloads", CORE),
    ("wait_for_download", CORE),
    // browse only (34)
    ("browser_hover", BROWSE),
    ("browser_scroll_element", BROWSE),
    ("browser_draw", BROWSE),
    ("browser_paste", BROWSE),
    ("browser_tabs", BROWSE),
    ("browser_file_upload", BROWSE),
    ("browser_history", BROWSE),
    ("browser_click_modified", BROWSE),
    ("browser_drag", BROWSE),
    ("browser_select_option", BROWSE),
    ("browser_hover_selector", BROWSE),
    ("browser_screenshot", BROWSE),
    ("set_viewport", BROWSE),
    ("dialog_set_auto_dismiss", BROWSE),
    ("dialog_clear_history", BROWSE),
    ("dialog_inject_override", BROWSE),
    ("dom_set_attr", BROWSE),
    ("dom_set_style", BROWSE),
    ("dom_set_text", BROWSE),
    ("dom_remove", BROWSE),
    ("vision_judge", BROWSE),
    ("console_clear", BROWSE),
    ("local_storage_get", BROWSE),
    ("local_storage_set", BROWSE),
    ("local_storage_delete", BROWSE),
    ("local_storage_clear", BROWSE),
    ("session_storage_get", BROWSE),
    ("session_storage_set", BROWSE),
    ("session_storage_delete", BROWSE),
    ("session_storage_clear", BROWSE),
    ("sw_list", BROWSE),
    ("sw_unregister", BROWSE),
    ("sw_caches", BROWSE),
    ("inspect_listeners", BROWSE),
    // browse ∩ reverse (19)
    ("list_endpoints", BROWSE_REVERSE),
    ("console_eval", BROWSE_REVERSE),
    ("console_get_props", BROWSE_REVERSE),
    ("cookie_list", BROWSE_REVERSE),
    ("cookie_set", BROWSE_REVERSE),
    ("cookie_delete", BROWSE_REVERSE),
    ("list_macros", BROWSE_REVERSE),
    ("get_macro", BROWSE_REVERSE),
    ("create_macro", BROWSE_REVERSE),
    ("update_macro", BROWSE_REVERSE),
    ("remove_macro_steps", BROWSE_REVERSE),
    ("run_macro", BROWSE_REVERSE),
    ("delete_macro", BROWSE_REVERSE),
    ("cli_arm", BROWSE_REVERSE),
    ("cli_compile", BROWSE_REVERSE),
    ("cli_list", BROWSE_REVERSE),
    ("cli_describe", BROWSE_REVERSE),
    ("cli_run", BROWSE_REVERSE),
    ("cli_writes", BROWSE_REVERSE),
    ("cli_export", BROWSE_REVERSE),
    // reverse, not browse (63). The four shared with probe are listed next.
    ("get_request_initiator", REVERSE),
    // Red Team exhaustive-click crawler — the deterministic UI sweep that feeds
    // discovered endpoints into the probe phase.
    ("ui_crawl", REVERSE),
    ("finding_remove", REVERSE),
    ("redteam_alt_session_set", REVERSE),
    ("list_scripts", REVERSE),
    ("grep_script", REVERSE),
    ("grep_scripts", REVERSE),
    ("extract_context", REVERSE),
    ("detect_bundler", REVERSE),
    ("deobfuscate_script", REVERSE),
    ("resolve_source", REVERSE),
    ("list_sources", REVERSE),
    ("get_original_source", REVERSE),
    ("deob_detect", REVERSE),
    ("deob_auto", REVERSE),
    ("list_wasm", REVERSE),
    ("wasm_decompile", REVERSE),
    ("wasm_info", REVERSE),
    ("grep_wasm", REVERSE),
    ("wasm_xref", REVERSE),
    ("crypto_chain", REVERSE),
    ("hash_iter", REVERSE),
    ("hmac_compute", REVERSE),
    ("magic_hash_lookup", REVERSE),
    ("crypto_trace_start", REVERSE),
    ("crypto_trace_list", REVERSE),
    ("crypto_trace_stop", REVERSE),
    ("exec_trace", REVERSE),
    ("inject_add", REVERSE),
    ("inject_list", REVERSE),
    ("inject_remove", REVERSE),
    ("inject_toggle", REVERSE),
    ("inject_run_now", REVERSE),
    ("intercept_add", REVERSE),
    ("intercept_list", REVERSE),
    ("intercept_remove", REVERSE),
    ("intercept_pending", REVERSE),
    ("intercept_continue", REVERSE),
    ("intercept_fulfill", REVERSE),
    ("intercept_fail", REVERSE),
    ("override_add", REVERSE),
    ("override_list", REVERSE),
    ("override_remove", REVERSE),
    ("header_preset_list", REVERSE),
    ("header_preset_save", REVERSE),
    ("header_preset_apply", REVERSE),
    ("header_preset_disable", REVERSE),
    ("list_websockets", REVERSE),
    ("get_ws_frames", REVERSE),
    ("ws_capture_enable", REVERSE),
    ("ws_capture_list", REVERSE),
    ("ws_send", REVERSE),
    ("fetch_body", REVERSE),
    ("decode_token", REVERSE),
    ("protobuf_decode", REVERSE),
    ("graphql_introspect", REVERSE),
    ("request_diff", REVERSE),
    ("find_api_base", REVERSE),
    ("auth_dump", REVERSE),
    ("export_python_client", REVERSE),
    ("export_client", REVERSE),
    ("har_export", REVERSE),
    ("import_har", REVERSE),
    ("detect_antibot_vm", REVERSE),
    // reverse ∩ probe (4)
    ("replay_request", REVERSE_PROBE),
    ("repeater_send", REVERSE_PROBE),
    ("scan_secrets", REVERSE_PROBE),
    ("security_inspect", REVERSE_PROBE),
    // probe only (6) — ALWAYS_RISKY_TOOLS
    ("intruder_run", PROBE),
    ("burst_send", PROBE),
    ("payload_probe", PROBE),
    ("crlf_test", PROBE),
    ("ssti_probe", PROBE),
    ("cmd_probe", PROBE),
    ("sqli_probe", PROBE),
    ("nosqli_probe", PROBE),
    ("idor_probe", PROBE),
    ("noauth_probe", PROBE),
    ("path_write_probe", PROBE),
    ("grpc_call", PROBE),
    ("pickle_gadget", PROBE),
    ("raw_http", PROBE),
    ("path_probe", PROBE),
    ("lfi_probe", PROBE),
    // hidden (9) — external catalog only
    ("bp_add", HIDDEN),
    ("bp_list", HIDDEN),
    ("bp_remove", HIDDEN),
    ("bp_status", HIDDEN),
    ("bp_resume", HIDDEN),
    ("bp_step_over", HIDDEN),
    ("bp_step_into", HIDDEN),
    ("bp_step_out", HIDDEN),
    ("bp_eval_in_frame", HIDDEN),
];

pub const PROFILE_TOOL_BUDGET: usize = 120;

pub fn tool_bucket(name: &str) -> Option<BucketSpec> {
    TOOL_BUCKETS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, spec)| *spec)
}

pub fn is_tool_profile(value: &Value) -> bool {
    value.as_str().and_then(ToolProfile::parse).is_some()
}

pub fn is_advertised(name: &str, profile: ToolProfile) -> bool {
    tool_bucket(name).is_some_and(|spec| spec.advertises(profile))
}

/// Names advertised for `profile`. `extras` are names this session already
/// passed to mcp.callTool. Refusals must not be included by the caller.
/// Hidden and unknown extras are dropped.
pub fn advertised_names<'a>(
    profile: ToolProfile,
    extras: impl IntoIterator<Item = &'a str>,
) -> Vec<&'a str> {
    let mut names: Vec<&'a str> = TOOL_BUCKETS
        .iter()
        .filter(|(_, spec)| spec.advertises(profile))
        .map(|(name, _)| *name)
        .collect();
    let mut seen: HashSet<&str> = names.iter().copied().collect();
    for name in extras {
        if seen.contains(name) || tool_bucket(name).is_none_or(BucketSpec::is_hidden) {
            continue;
        }
        seen.insert(name);
        names.push(name);
    }
    names
}

pub fn assert_profile_budget(profile: ToolProfile) -> Result<(), String> {
    check_budget(profile, advertised_names(profile, []).len())
}

fn check_budget(profile: ToolProfile, n: usize) -> Result<(), String> {
    if n > PROFILE_TOOL_BUDGET {
        return Err(format!(
            "tool profile {profile} advertises {n} tools, budget is {PROFILE_TOOL_BUDGET}"
        ));
    }
    Ok(())
}

/// The two fields of a tool-profile.json body that are read. Every other value
/// is skipped without being built: JSON.parse accepts some that
/// `serde_json::Value` refuses (a lone surrogate escape, a number past f64,
/// nesting deeper than 128), and they must not void the profile.
struct ProfileRow {
    version: Value,
    profile: Value,
}

impl<'de> Deserialize<'de> for ProfileRow {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RowVisitor;

        impl<'de> Visitor<'de> for RowVisitor {
            type Value = ProfileRow;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a tool-profile.json object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ProfileRow, A::Error> {
                let mut row = ProfileRow {
                    version: Value::Null,
                    profile: Value::Null,
                };
                while let Some(key) = map.next_key::<String>()? {
                    // A repeated key overwrites the earlier one, as in JSON.parse.
                    match key.as_str() {
                        "version" => row.version = map.next_value()?,
                        "profile" => row.profile = map.next_value()?,
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(row)
            }
        }

        deserializer.deserialize_map(RowVisitor)
    }
}

/// Parse a tool-profile.json body. Does not write. Invalid input returns `None`.
pub fn read_tool_profile(json: &str) -> Option<ToolProfile> {
    let row: ProfileRow = serde_json::from_str(json).ok()?;
    // JSON has one number type, so `1.0` is version 1 as it is in JavaScript.
    if row.version.as_f64() != Some(1.0) {
        return None;
    }
    row.profile.as_str().and_then(ToolProfile::parse)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CatalogKind {
    Full,
    Follow,
    Direct,
}

/// Header value must be exactly `full` or `direct`; anything else, including no
/// header, is `follow`. An external client (the /rever skill, a hand-added
/// Claude Code or Codex) gets the in-app profile, not all ~160 tool schemas;
/// in-app callers that need every tool (macros, workflows, revb, e2e)
/// send `full` explicitly.
///
/// Takes the header's values: `Some("full")`, a repeated header's list (only
/// the first value counts), or `None`.
pub fn catalog_from_header<'a>(value: impl IntoIterator<Item = &'a str>) -> CatalogKind {
    match value.into_iter().next() {
        Some("full") => CatalogKind::Full,
        Some("direct") => CatalogKind::Direct,
        _ => CatalogKind::Follow,
    }
}

pub const TOOL_PROFILE_SET_DESCRIPTION: &str = "Switch the in-app tool profile, or omit profile to read it.\n\
browse (default): drive the page, read the DOM, and use the site CLI (cli_compile / cli_run / cli_export).\n\
reverse: scripts, source maps, deobfuscation, wasm, crypto_trace, intercept/replay, codegen (export_python_client / export_client), site CLI, websockets.\n\
probe: intruder_run, burst_send, payload_probe, crlf_test, ssti_probe, path_probe, lfi_probe. You cannot select probe; ask the user to set Tools → Access.\n\
bp_* breakpoints are not available in any profile.";

pub fn tool_profile_read_text(profile: ToolProfile, advertised: usize) -> String {
    format!(
        "profile={profile} advertised={advertised}. Switch with profile \"browse\" or \"reverse\". You cannot select probe; the user sets Tools → Access. bp_* is not in any profile."
    )
}

pub fn tool_profile_switched_text(profile: ToolProfile, advertised: usize) -> String {
    format!(
        "profile={profile} advertised={advertised}. If grep_scripts is not in your tool list yet, ask the user to send one more message."
    )
}

pub const TOOL_PROFILE_PROBE_REJECTED: &str =
    "probe is user-selected. Ask the user to set Tools → Access. Profile was not changed.";

/// Live registered-tool handle. `enabled` is the bit `apply_follow_catalog` diffs.
pub trait ToolToggle {
    fn enabled(&self) -> bool;
    fn enable(&mut self);
    fn disable(&mut self);
}

/// The MCP server side of `apply_follow_catalog`.
///
/// The TypeScript swaps `sendToolListChanged` for a no-op while it toggles,
/// because each handle's enable/disable sends the notification itself. A trait
/// method cannot be swapped, so the server exposes the mute as a switch:
/// while muted, `send_tool_list_changed` must do nothing.
pub trait ToolListServer {
    fn is_connected(&self) -> bool;
    fn send_tool_list_changed(&self);
    fn set_tool_list_changed_muted(&self, muted: bool);
}

/// Only follow sessions disable tools. full stays complete for callers that ask for it.
/// direct stays complete because the provider filters each turn instead.
pub fn catalog_disables_tools(catalog: CatalogKind) -> bool {
    catalog == CatalogKind::Follow
}

/// Unmutes on drop, so a handle that panics mid-toggle does not leave the
/// server silenced (the TypeScript restores in `finally`).
struct Unmute<'a, S: ToolListServer + ?Sized>(&'a S);

impl<S: ToolListServer + ?Sized> Drop for Unmute<'_, S> {
    fn drop(&mut self) {
        self.0.set_tool_list_changed_muted(false);
    }
}

/// Enable the tools `profile` advertises and disable the rest.
/// While toggling, `send_tool_list_changed` is muted. If any bit flipped and the
/// server is connected, the notification runs once. No bit change, or
/// a server that is not connected yet, sends nothing.
pub fn apply_follow_catalog<'a, S, N, T>(
    mcp: &S,
    handles: impl IntoIterator<Item = (N, &'a mut T)>,
    profile: ToolProfile,
) where
    S: ToolListServer + ?Sized,
    N: AsRef<str>,
    T: ToolToggle + ?Sized + 'a,
{
    mcp.set_tool_list_changed_muted(true);
    let unmute = Unmute(mcp);
    let mut changed = false;
    for (name, handle) in handles {
        let want = is_advertised(name.as_ref(), profile);
        if want == handle.enabled() {
            continue;
        }
        if want {
            handle.enable();
        } else {
            handle.disable();
        }
        changed = true;
    }
    drop(unmute);
    if changed && mcp.is_connected() {
        mcp.send_tool_list_changed();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;
    use std::rc::Rc;

    use regex::Regex;

    use super::*;
    use crate::risky_tools::ALWAYS_RISKY_TOOLS;

    const BROWSE_REVERSE_NAMES: [&str; 20] = [
        "list_endpoints",
        "console_eval",
        "console_get_props",
        "cookie_list",
        "cookie_set",
        "cookie_delete",
        "list_macros",
        "get_macro",
        "create_macro",
        "update_macro",
        "remove_macro_steps",
        "run_macro",
        "delete_macro",
        "cli_arm",
        "cli_compile",
        "cli_list",
        "cli_describe",
        "cli_run",
        "cli_writes",
        "cli_export",
    ];

    const HIDDEN_NAMES: [&str; 9] = [
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

    fn sorted<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        let mut names: Vec<&str> = names.into_iter().collect();
        names.sort_unstable();
        names
    }

    fn registry_keys() -> Vec<&'static str> {
        TOOL_BUCKETS.iter().map(|(name, _)| *name).collect()
    }

    /// Source `registerTool` names, with storage.ts `kind` templates expanded.
    fn source_tool_names() -> BTreeSet<String> {
        // The TypeScript app is gone; this is the frozen registerTool name
        // set of its final state (snapshot taken from history, 177 names).
        let order: Vec<String> =
            serde_json::from_str(&fs::read_to_string(snapshot("ts-tool-order.json")).unwrap())
                .unwrap();
        order.into_iter().collect()
    }

    /// A file under the crate's snapshot/ — the TypeScript originals the port
    /// was checked against, kept verbatim after the app's removal.
    fn snapshot(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("snapshot")
            .join(name)
    }

    #[test]
    fn 소스_register_tool_이름과_레지스트리_키가_같다() {
        let source = source_tool_names();
        assert_eq!(
            sorted(registry_keys()),
            sorted(source.iter().map(String::as_str))
        );
        assert_eq!(source.len(), 177);
        assert!(source.contains("tool_profile_set"));
    }

    // Not in the TypeScript tests: the counts above do not pin which bucket a
    // name is in, or the order `advertised_names` lists them in.
    #[test]
    fn 레지스트리는_타입스크립트_원문과_이름_버킷_순서가_같다() {
        let source = fs::read_to_string(snapshot("tool-buckets.ts")).unwrap();
        let start = source.find("export const TOOL_BUCKETS").unwrap();
        let end = start + source[start..].find("\n}\n").unwrap();
        let entry = Regex::new(r"(?m)^  (\w+): ('core'|\[[^\]]*\]),?$").unwrap();
        let ts: Vec<(&str, &str)> = entry
            .captures_iter(&source[start..end])
            .map(|m| {
                let (_, [name, bucket]) = m.extract();
                (name, bucket)
            })
            .collect();
        let rust: Vec<(&str, String)> = TOOL_BUCKETS
            .iter()
            .map(|(name, spec)| {
                let bucket = match spec {
                    BucketSpec::Core => "'core'".to_owned(),
                    BucketSpec::Of(buckets) => serde_json::to_string(buckets)
                        .unwrap()
                        .replace('"', "'")
                        .replace(',', ", "),
                };
                (*name, bucket)
            })
            .collect();
        assert_eq!(ts.len(), 177);
        assert_eq!(
            rust.iter()
                .map(|(n, b)| (*n, b.as_str()))
                .collect::<Vec<_>>(),
            ts
        );
    }

    #[test]
    fn browse_84_reverse_118_probe_50이고_예산_120을_넘지_않는다() {
        assert_eq!(PROFILE_TOOL_BUDGET, 120);
        assert_eq!(advertised_names(ToolProfile::Browse, []).len(), 84);
        assert_eq!(advertised_names(ToolProfile::Reverse, []).len(), 118);
        assert_eq!(advertised_names(ToolProfile::Probe, []).len(), 50);
        for profile in TOOL_PROFILES {
            assert_eq!(assert_profile_budget(profile), Ok(()));
        }
    }

    #[test]
    fn 코어_도구는_세_프로파일에_모두_있고_레지스트리에_없는_이름은_빠진다() {
        assert!(is_advertised("browser_snapshot", ToolProfile::Browse));
        assert!(is_advertised("browser_snapshot", ToolProfile::Reverse));
        assert!(is_advertised("browser_snapshot", ToolProfile::Probe));
        assert!(!is_advertised("grep_scripts", ToolProfile::Browse));
        assert!(is_advertised("grep_scripts", ToolProfile::Reverse));
        assert!(!is_advertised("not_a_tool", ToolProfile::Browse));
    }

    #[test]
    fn 코어를_뺀_browse와_reverse의_겹침은_20개_이름으로_고정된다() {
        let overlap = TOOL_BUCKETS
            .iter()
            .filter(|(name, spec)| {
                *spec != BucketSpec::Core
                    && is_advertised(name, ToolProfile::Browse)
                    && is_advertised(name, ToolProfile::Reverse)
            })
            .map(|(name, _)| *name);
        assert_eq!(sorted(overlap), sorted(BROWSE_REVERSE_NAMES));
    }

    #[test]
    fn always_risky_tools_6개는_probe에만_있다() {
        assert_eq!(ALWAYS_RISKY_TOOLS.len(), 6);
        for name in ALWAYS_RISKY_TOOLS {
            assert!(is_advertised(name, ToolProfile::Probe), "{name}");
            assert!(!is_advertised(name, ToolProfile::Browse), "{name}");
            assert!(!is_advertised(name, ToolProfile::Reverse), "{name}");
        }
    }

    #[test]
    fn bp_9개는_hidden이고_어떤_프로파일에도_광고되지_않는다() {
        let hidden = TOOL_BUCKETS
            .iter()
            .filter(|(_, spec)| spec.is_hidden())
            .map(|(name, _)| *name);
        assert_eq!(sorted(hidden), sorted(HIDDEN_NAMES));
        for name in HIDDEN_NAMES {
            for profile in TOOL_PROFILES {
                assert!(!is_advertised(name, profile), "{name} {profile}");
            }
        }
    }

    #[test]
    fn 세_프로파일의_광고_합은_hidden을_제외한_전부다() {
        let mut advertised: HashSet<&str> = HashSet::new();
        for profile in TOOL_PROFILES {
            advertised.extend(advertised_names(profile, []));
        }
        let hidden: HashSet<&str> = HIDDEN_NAMES.into_iter().collect();
        assert_eq!(advertised.len() + hidden.len(), TOOL_BUCKETS.len());
        for name in registry_keys() {
            assert!(advertised.contains(name) || hidden.contains(name), "{name}");
            assert!(
                !(advertised.contains(name) && hidden.contains(name)),
                "{name}"
            );
        }
    }

    #[test]
    fn extras에서_hidden과_모르는_이름은_빼고_다른_프로파일에서_이미_호출된_이름은_붙인다() {
        let names = advertised_names(
            ToolProfile::Browse,
            [
                "grep_scripts",
                "bp_add",
                "not_a_tool",
                "browser_snapshot",
                "grep_scripts",
            ],
        );
        assert!(names.contains(&"grep_scripts"));
        assert!(!names.contains(&"bp_add"));
        assert!(!names.contains(&"not_a_tool"));
        assert_eq!(
            names.iter().filter(|n| **n == "browser_snapshot").count(),
            1
        );
        assert_eq!(names.iter().filter(|n| **n == "grep_scripts").count(), 1);
        assert_eq!(names.len(), 85);
    }

    #[test]
    fn version이_1이고_profile이_세_값이면_그_프로파일을_반환한다() {
        assert_eq!(
            read_tool_profile(r#"{"version":1,"profile":"browse"}"#),
            Some(ToolProfile::Browse)
        );
        assert_eq!(
            read_tool_profile(r#"{"version":1,"profile":"reverse","extra":true}"#),
            Some(ToolProfile::Reverse)
        );
        assert_eq!(
            read_tool_profile(r#"{"version":1,"profile":"probe"}"#),
            Some(ToolProfile::Probe)
        );
    }

    #[test]
    fn 깨진_json_다른_version_알_수_없는_profile은_none이다() {
        assert_eq!(read_tool_profile(""), None);
        assert_eq!(read_tool_profile("{"), None);
        assert_eq!(read_tool_profile("null"), None);
        assert_eq!(read_tool_profile("[]"), None);
        assert_eq!(
            read_tool_profile(r#"{"version":2,"profile":"browse"}"#),
            None
        );
        assert_eq!(
            read_tool_profile(r#"{"version":"1","profile":"browse"}"#),
            None
        );
        assert_eq!(read_tool_profile(r#"{"version":1,"profile":"full"}"#), None);
        assert_eq!(read_tool_profile(r#"{"version":1}"#), None);
        assert_eq!(read_tool_profile(r#"{"profile":"browse"}"#), None);
    }

    // Not in the TypeScript tests: JSON.parse reads 1.0 as the number 1, and
    // scalars or arrays never carry a version.
    #[test]
    fn version_1점0은_1로_읽고_객체가_아닌_json은_none이다() {
        assert_eq!(
            read_tool_profile(r#"{"version":1.0,"profile":"browse"}"#),
            Some(ToolProfile::Browse)
        );
        assert_eq!(
            read_tool_profile(r#"{"version":1.5,"profile":"browse"}"#),
            None
        );
        assert_eq!(
            read_tool_profile(r#"{"version":true,"profile":"browse"}"#),
            None
        );
        assert_eq!(read_tool_profile(r#"{"version":1,"profile":3}"#), None);
        assert_eq!(read_tool_profile(r#""browse""#), None);
        assert_eq!(read_tool_profile("1"), None);
        assert_eq!(
            read_tool_profile(r#"[{"version":1,"profile":"browse"}]"#),
            None
        );
    }

    // Not in the TypeScript tests: bodies JSON.parse accepts (checked with bun).
    #[test]
    fn 읽지_않는_필드의_값이_특이해도_프로파일을_읽고_반복된_키는_마지막_값을_쓴다() {
        let deep = format!(
            r#"{{"version":1,"profile":"browse","n":{}{}}}"#,
            "[".repeat(200),
            "]".repeat(200)
        );
        for body in [
            r#"{"version":1,"profile":"browse","x":"\ud800"}"#,
            r#"{"version":1,"profile":"browse","s":"\ude00\ud83d"}"#,
            r#"{"version":1,"profile":"browse","n":1e999}"#,
            r#"{"version":100000000000000000000e-20,"profile":"browse"}"#,
            " {\"version\":1,\"profile\":\"browse\"} \n",
            deep.as_str(),
        ] {
            assert_eq!(read_tool_profile(body), Some(ToolProfile::Browse), "{body}");
        }
        assert_eq!(
            read_tool_profile(r#"{"version":2,"profile":"probe","version":1}"#),
            Some(ToolProfile::Probe)
        );
        assert_eq!(
            read_tool_profile(r#"{"version":1,"profile":"x","profile":"reverse"}"#),
            Some(ToolProfile::Reverse)
        );
        for body in [
            r#"{"version":1,"profile":"probe","version":2}"#,
            r#"{"version":1e400,"profile":"browse"}"#,
            r#"{"version":1,"profile":"browse",}"#,
            r#"{"version":1,"profile":"browse"}x"#,
            r#"{"version":01,"profile":"browse"}"#,
            r#"{"version":1,"profile":"browse","n":00}"#,
            r#"{"version":1,"profile":"browse","s":"\x41"}"#,
            r#"{"version":1,"profile":["browse"]}"#,
            r#"{'version':1,'profile':'browse'}"#,
            "\u{feff}{\"version\":1,\"profile\":\"browse\"}",
            "{\"version\":1,\"profile\":\"browse\"}\u{a0}",
            "{\"version\":1,\"profile\":\"browse\",\"t\":\"a\tb\"}",
        ] {
            assert_eq!(read_tool_profile(body), None, "{body}");
        }
    }

    #[test]
    fn is_tool_profile은_세_문자열만_받는다() {
        assert!(is_tool_profile(&Value::from("browse")));
        assert!(is_tool_profile(&Value::from("reverse")));
        assert!(is_tool_profile(&Value::from("probe")));
        assert!(!is_tool_profile(&Value::from("hidden")));
        assert!(!is_tool_profile(&Value::from("Probe")));
        assert!(!is_tool_profile(&Value::Null));
        assert!(!is_tool_profile(&Value::from(1)));
    }

    #[test]
    fn 헤더는_full과_direct만_받고_없거나_나머지는_follow다() {
        assert_eq!(catalog_from_header(Some("follow")), CatalogKind::Follow);
        assert_eq!(catalog_from_header(Some("direct")), CatalogKind::Direct);
        assert_eq!(catalog_from_header(Some("full")), CatalogKind::Full);
        assert_eq!(catalog_from_header(None), CatalogKind::Follow);
        assert_eq!(catalog_from_header(Some("Full")), CatalogKind::Follow);
        assert_eq!(catalog_from_header(Some("DIRECT")), CatalogKind::Follow);
        assert_eq!(catalog_from_header(Some("")), CatalogKind::Follow);
        assert_eq!(catalog_from_header(["direct"]), CatalogKind::Direct);
        assert_eq!(catalog_from_header(["full"]), CatalogKind::Full);
    }

    #[test]
    fn 매_턴_한_줄은_프로파일_이름만_바뀐다() {
        assert_eq!(
            tool_profile_turn_line(ToolProfile::Browse),
            "[tool-profile: browse] Tools outside this profile are not loaded. Call tool_profile_set to switch between browse and reverse. Probe is selected by the user."
        );
        assert!(tool_profile_turn_line(ToolProfile::Reverse).contains("[tool-profile: reverse]"));
        assert!(tool_profile_turn_line(ToolProfile::Probe).contains("[tool-profile: probe]"));
    }

    #[test]
    fn 읽기_전환_probe_거부_문자열이_고정이다() {
        assert_eq!(
            tool_profile_read_text(ToolProfile::Browse, 77),
            "profile=browse advertised=77. Switch with profile \"browse\" or \"reverse\". You cannot select probe; the user sets Tools → Access. bp_* is not in any profile."
        );
        assert_eq!(
            tool_profile_switched_text(ToolProfile::Reverse, 109),
            "profile=reverse advertised=109. If grep_scripts is not in your tool list yet, ask the user to send one more message."
        );
        assert_eq!(
            TOOL_PROFILE_PROBE_REJECTED,
            "probe is user-selected. Ask the user to set Tools → Access. Profile was not changed."
        );
    }

    // Not in the TypeScript tests: the description is sent to the model verbatim.
    #[test]
    fn tool_profile_set_설명은_다섯_줄이고_타입스크립트_원문과_같다() {
        let source = fs::read_to_string(snapshot("tool-profile-set-description.txt")).unwrap();
        assert_eq!(
            TOOL_PROFILE_SET_DESCRIPTION,
            source.trim_end_matches(['\n', '\r'])
        );
        assert_eq!(TOOL_PROFILE_SET_DESCRIPTION.lines().count(), 5);
    }

    #[test]
    fn 예산을_넘으면_프로파일과_개수를_담은_오류를_낸다() {
        assert_eq!(check_budget(ToolProfile::Reverse, 120), Ok(()));
        assert_eq!(
            check_budget(ToolProfile::Reverse, 121),
            Err("tool profile reverse advertises 121 tools, budget is 120".to_owned())
        );
    }

    // --- src/main/mcp/tool-profile-apply.test.ts ---

    struct Mcp {
        connected: bool,
        muted: Cell<bool>,
        sends: Cell<u32>,
    }

    impl ToolListServer for Mcp {
        fn is_connected(&self) -> bool {
            self.connected
        }
        fn send_tool_list_changed(&self) {
            if !self.muted.get() {
                self.sends.set(self.sends.get() + 1);
            }
        }
        fn set_tool_list_changed_muted(&self, muted: bool) {
            self.muted.set(muted);
        }
    }

    /// Like the SDK's RegisteredTool: every toggle notifies the server itself.
    struct Handle {
        enabled: bool,
        mcp: Rc<Mcp>,
    }

    impl ToolToggle for Handle {
        fn enabled(&self) -> bool {
            self.enabled
        }
        fn enable(&mut self) {
            self.enabled = true;
            self.mcp.send_tool_list_changed();
        }
        fn disable(&mut self) {
            self.enabled = false;
            self.mcp.send_tool_list_changed();
        }
    }

    struct Harness {
        mcp: Rc<Mcp>,
        handles: Vec<(&'static str, Handle)>,
    }

    impl Harness {
        fn new(connected: bool) -> Self {
            Self {
                mcp: Rc::new(Mcp {
                    connected,
                    muted: Cell::new(false),
                    sends: Cell::new(0),
                }),
                handles: Vec::new(),
            }
        }
        fn add(&mut self, name: &'static str, enabled: bool) {
            let mcp = Rc::clone(&self.mcp);
            self.handles.push((name, Handle { enabled, mcp }));
        }
        fn apply(&mut self, profile: ToolProfile) {
            apply_follow_catalog(
                &*self.mcp,
                self.handles
                    .iter_mut()
                    .map(|(name, handle)| (*name, handle)),
                profile,
            );
        }
        fn enabled(&self, name: &str) -> Option<bool> {
            self.handles
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, handle)| handle.enabled)
        }
        fn sends(&self) -> u32 {
            self.mcp.sends.get()
        }
    }

    #[test]
    fn 연결된_서버에서_토글이_여러_번이어도_list_changed는_한_번이다() {
        let mut h = Harness::new(true);
        h.add("browser_snapshot", true);
        h.add("grep_scripts", true);
        h.add("intruder_run", true);
        h.add("bp_add", true);

        h.apply(ToolProfile::Browse);

        assert_eq!(h.sends(), 1);
        assert_eq!(h.enabled("browser_snapshot"), Some(true));
        assert_eq!(h.enabled("grep_scripts"), Some(false));
        assert_eq!(h.enabled("intruder_run"), Some(false));
        assert_eq!(h.enabled("bp_add"), Some(false));
        h.mcp.send_tool_list_changed();
        assert_eq!(h.sends(), 2);
    }

    #[test]
    fn 비트가_그대로면_list_changed를_보내지_않는다() {
        let mut h = Harness::new(true);
        h.add("browser_snapshot", true);
        h.add("grep_scripts", false);
        h.add("bp_add", false);

        h.apply(ToolProfile::Browse);

        assert_eq!(h.sends(), 0);
        assert_eq!(h.enabled("browser_snapshot"), Some(true));
        assert_eq!(h.enabled("grep_scripts"), Some(false));
    }

    #[test]
    fn 연결_전에는_비트가_바뀌어도_list_changed를_보내지_않는다() {
        let mut h = Harness::new(false);
        h.add("grep_scripts", true);

        h.apply(ToolProfile::Browse);

        assert_eq!(h.sends(), 0);
        assert_eq!(h.enabled("grep_scripts"), Some(false));
    }

    #[test]
    fn follow만_도구를_끄고_full과_direct는_끄지_않는다() {
        assert!(catalog_disables_tools(CatalogKind::Follow));
        assert!(!catalog_disables_tools(CatalogKind::Full));
        assert!(!catalog_disables_tools(CatalogKind::Direct));
    }

    // Not in the TypeScript tests: a disabled tool the profile wants is turned on.
    #[test]
    fn 꺼져_있던_광고_대상_도구는_켜고_레지스트리에_없는_이름은_끈다() {
        let mut h = Harness::new(true);
        h.add("grep_scripts", false);
        h.add("not_a_tool", true);

        h.apply(ToolProfile::Reverse);

        assert_eq!(h.enabled("grep_scripts"), Some(true));
        assert_eq!(h.enabled("not_a_tool"), Some(false));
        assert_eq!(h.sends(), 1);
    }
}
