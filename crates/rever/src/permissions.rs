//! Which tool calls of the chat agent the user is asked about.
//!
//! The app's own tools follow the shared risky-tool rules (`rever_tool_profile`). A
//! tool from outside the app — the CLI's shell, its file tools — acts on this machine
//! rather than on the page, so it is always asked about, whatever auto-approve says.

use rever_agent::claude_cli_protocol::MCP_SERVER_NAME;
use rever_chat_core::RiskyTools;
use rever_ui_chat::permission_prompt::ToolRules;
use serde_json::Value;

use crate::agent::SESSION_ID;

/// The rules the chat's permission queue decides by.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChatRules;

/// The name of the tool a Claude CLI request is for, when it is not one of the app's.
/// Requests of other agents (Codex over ACP) carry a title, not a tool name, and are
/// left to the shared rules.
fn outside_tool(request: &Value) -> Option<String> {
    if request["sessionId"] != SESSION_ID {
        return None;
    }
    let name = request.pointer("/toolCall/title")?.as_str()?;
    let own = name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.strip_prefix(MCP_SERVER_NAME))
        .is_some_and(|rest| rest.starts_with("__"));
    // ToolSearch only loads tool descriptions.
    (!own && name != "ToolSearch").then(|| name.to_owned())
}

impl RiskyTools for ChatRules {
    fn shell_http_tool(&self) -> &str {
        ToolRules.shell_http_tool()
    }

    fn matched_risky_tool(&self, request: &Value) -> Option<String> {
        ToolRules
            .matched_risky_tool(request)
            .or_else(|| outside_tool(request))
    }

    // A gated tool is never answered "always allow": the grant lives in the queue.
    fn gated_tool_name(&self, request: &Value) -> Option<String> {
        ToolRules
            .gated_tool_name(request)
            .or_else(|| outside_tool(request))
    }

    fn extract_target_host(&self, raw_input: Option<&Value>) -> Option<String> {
        ToolRules.extract_target_host(raw_input)
    }

    fn choose_allow_option(&self, request: &Value) -> String {
        ToolRules.choose_allow_option(request)
    }

    fn find_reject_option(&self, request: &Value) -> String {
        ToolRules.find_reject_option(request)
    }

    fn sanitize_selection(&self, request: &Value, option_id: &str) -> String {
        ToolRules.sanitize_selection(request, option_id)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use rever_agent::claude_cli_protocol::to_permission_request;
    use rever_chat_core::acp_permission::{PermissionStore, RequestPermissionResponse};
    use rever_chat_core::red_team_mode::RedTeamMode;
    use serde_json::json;

    use super::*;

    /// The Claude CLI's request for `tool`, as the queue receives it.
    fn cli_request(tool: &str, input: Value) -> Value {
        to_permission_request(SESSION_ID, &json!({ "tool_name": tool, "input": input }))
    }

    /// Hands `request` to a fresh queue: whether it waits for the user, and the answer
    /// given so far.
    fn ask(request: Value) -> (bool, Option<RequestPermissionResponse>) {
        let mut store = PermissionStore::new(ChatRules, |_| {});
        let answer = Arc::new(Mutex::new(None));
        let given = answer.clone();
        let waits = store
            .request_permission_from_user(request, &RedTeamMode::new(), move |response| {
                *given.lock().unwrap() = Some(response);
            })
            .is_some();
        let answer = answer.lock().unwrap().take();
        (waits, answer)
    }

    #[test]
    fn 앱의_일반_도구는_묻지_않고_허용한다() {
        let (waits, answer) = ask(cli_request("mcp__rever-traffic__list_requests", json!({})));
        assert!(!waits);
        assert!(answer.is_some(), "the call was not answered");
    }

    #[test]
    fn 앱의_위험_도구는_사용자에게_묻는다() {
        let (waits, answer) = ask(cli_request("mcp__rever-traffic__intruder_run", json!({})));
        assert!(waits);
        assert_eq!(answer, None);
    }

    #[test]
    fn 앱_밖의_도구는_자동_승인이어도_묻는다() {
        let (waits, _) = ask(cli_request("Bash", json!({ "command": "ls" })));
        assert!(waits);
    }

    #[test]
    fn 도구_설명을_읽는_toolsearch는_묻지_않는다() {
        let (waits, _) = ask(cli_request("ToolSearch", json!({ "query": "macro" })));
        assert!(!waits);
    }

    #[test]
    fn 이름만_비슷한_다른_서버의_도구는_밖의_도구다() {
        let request = cli_request("mcp__rever-traffic-evil__list_requests", json!({}));
        assert_eq!(
            outside_tool(&request).as_deref(),
            Some("mcp__rever-traffic-evil__list_requests")
        );
    }

    #[test]
    fn 다른_에이전트의_요청은_밖의_도구로_보지_않는다() {
        let request = json!({ "sessionId": "codex-1", "toolCall": { "title": "Run ls" } });
        assert_eq!(outside_tool(&request), None);
    }
}
