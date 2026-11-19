//! Chat logic ported from src/renderer/src/ai: update mapping, permissions, Red Team modes, without GPUI

use serde_json::Value;

pub mod acp_background;
pub mod acp_map_update;
pub mod acp_permission;
pub mod format_error;
pub mod provider_notice;
pub mod red_team_mode;
pub mod redteam_categories;
pub mod session_grants;

#[cfg(test)]
mod test_risky_tools;

/// The seam for `src/shared/risky-tools.ts`, which is ported in another crate:
/// this crate does not depend on it, so the permission queue and the session
/// grants take the risky-tool decisions through this trait instead.
///
/// Requests are ACP `RequestPermissionRequest` values as rever-agent emits them
/// (`{ sessionId, toolCall: { toolCallId, title, rawInput }, options: [...] }`).
/// Each method has the meaning of the TypeScript function of the same name.
pub trait RiskyTools {
    /// `SHELL_HTTP_TOOL`: the pseudo tool id of a shell command that sends HTTP itself.
    fn shell_http_tool(&self) -> &str;
    /// The matched risky tool id, or `None` when the request may be auto-approved.
    fn matched_risky_tool(&self, req: &Value) -> Option<String>;
    /// The gated tool id (either list, whatever the method), or `None`.
    fn gated_tool_name(&self, req: &Value) -> Option<String>;
    /// The host a tool input targets, from the request's `toolCall.rawInput`.
    fn extract_target_host(&self, raw_input: Option<&Value>) -> Option<String>;
    fn choose_allow_option(&self, req: &Value) -> String;
    fn find_reject_option(&self, req: &Value) -> String;
    /// Downgrades an explicit "Always Allow" pick on a gated tool to allow_once.
    fn sanitize_selection(&self, req: &Value, option_id: &str) -> String;

    fn is_risky_tool_request(&self, req: &Value) -> bool {
        self.matched_risky_tool(req).is_some()
    }
}

/// JavaScript's whitespace set (`\s`, `String.prototype.trim`). Rust's differs
/// by two characters: U+FEFF is whitespace only in JS, U+0085 only in Rust.
pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}
