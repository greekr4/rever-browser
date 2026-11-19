//! Port of `src/main/providers/claude-cli-protocol.ts` — the Claude Code headless protocol
//! (`claude -p --input-format stream-json --output-format stream-json`), translated to the
//! ACP SessionNotification shape the UI already consumes. Pure functions only — the process
//! plumbing lives with the provider. Shapes verified against Claude Code 2.1.318 on 2026-11-06.
//!
//! ACP and CLI messages are `serde_json::Value`: the TypeScript builds and reads them as
//! loose records too. A field that would be `undefined` there is left out here.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MCP_SERVER_NAME: &str = "rever-traffic";

/// Tools that are plumbing, not user-visible work (deferred MCP schema loading).
const HIDDEN_TOOLS: &[&str] = &["ToolSearch"];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliModel {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_effort_levels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_fast_mode: Option<bool>,
}

/// Session knobs beyond the model, changed live with apply_flag_settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliOptions {
    pub effort: Option<String>,
    pub fast_mode: bool,
}

/// get_settings response → the effort/fast mode actually in force.
pub fn options_from_settings(r: &Value) -> CliOptions {
    CliOptions {
        effort: r
            .pointer("/applied/effort")
            .and_then(Value::as_str)
            .map(str::to_owned),
        fast_mode: r.pointer("/effective/fastMode") == Some(&Value::Bool(true)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FlagKey {
    Effort,
    FastMode,
}

/// One option change → the apply_flag_settings request body.
pub fn flag_settings_request(key: FlagKey, value: &Value) -> Result<Value, &'static str> {
    let settings = match key {
        FlagKey::Effort => {
            json!({ "effortLevel": value.as_str().ok_or("effort must be a level name")? })
        }
        FlagKey::FastMode => {
            json!({ "fastMode": value.as_bool().ok_or("fastMode must be true or false")? })
        }
    };
    Ok(json!({ "subtype": "apply_flag_settings", "settings": settings }))
}

pub fn claude_cli_args(mcp_url: &str, auth_header: &str, model: Option<&str>) -> Vec<String> {
    let mcp = json!({
        "mcpServers": {
            MCP_SERVER_NAME: {
                "type": "http",
                "url": mcp_url,
                "headers": { "Authorization": auth_header, "X-Rever-Catalog": "follow" }
            }
        }
    });
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--mcp-config",
        &mcp.to_string(),
        "--strict-mcp-config",
        // Same isolation the ACP path used (settingSources: []): no user hooks or settings.
        // Not --bare: that skips the keychain, which is where the subscription login lives.
        "--setting-sources",
        "",
        // No nested sub-agents: this app drives one tab per session, and a Task
        // sub-agent runs in its own context without that tab's CDP/ref state, so it
        // can't usefully help — in the Red Team scan its only effect was workers
        // burning 180-420s on a nested agent. Disallowing an absent tool is a no-op,
        // so this is safe even if the built-in name ever changes.
        "--disallowed-tools",
        "Task",
        "--permission-mode",
        "manual",
        // Route every permission prompt to us as a can_use_tool control_request.
        "--permission-prompt-tool",
        "stdio",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if let Some(model) = model.filter(|m| !m.is_empty()) {
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    args
}

pub fn line(o: &Value) -> String {
    format!("{o}\n")
}

pub fn user_message(text: &str) -> String {
    line(&json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] }
    }))
}

pub fn control_request(request_id: &str, request: &Value) -> String {
    line(&json!({ "type": "control_request", "request_id": request_id, "request": request }))
}

#[derive(Debug, Clone, Default)]
pub struct TranslateState {
    /// Ids of hidden tool calls, keyed by `id_key`.
    hidden: HashSet<String>,
}

impl TranslateState {
    fn is_hidden(&self, id: Option<&Value>) -> bool {
        self.hidden.contains(&id_key(id))
    }
}

/// A JS `Set` holds loosely typed ids, including a missing one; the JSON text keeps
/// those apart (`"ts"` vs `undefined`).
fn id_key(id: Option<&Value>) -> String {
    id.map_or_else(|| "undefined".to_owned(), Value::to_string)
}

/// JS truthiness of a field that may be missing.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `${value}` for a loosely typed field.
// shortcut: a float prints as serde_json writes it (`1.0`, where JS says `1`);
// upgrade if the CLI ever sends non-integer numbers in a text or error slot.
fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => js_join(items, ","),
        Some(Value::Object(_)) => "[object Object]".to_owned(),
        Some(other) => other.to_string(),
    }
}

/// `Array.prototype.join`: null renders as an empty string.
fn js_join(items: &[Value], separator: &str) -> String {
    items
        .iter()
        .map(|item| match item {
            Value::Null => String::new(),
            item => js_string(Some(item)),
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// An object literal as `JSON.stringify` emits it: `undefined` (None) fields are left out.
fn object<const N: usize>(fields: [(&str, Option<Value>); N]) -> Value {
    Value::Object(
        fields
            .into_iter()
            .filter_map(|(key, value)| Some((key.to_owned(), value?)))
            .collect(),
    )
}

fn type_of(v: Option<&Value>) -> Option<&str> {
    v?.get("type")?.as_str()
}

fn note(session_id: &str, update: Value) -> Value {
    json!({ "sessionId": session_id, "update": update })
}

fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|c| match type_of(Some(c)) {
                // `filter(Boolean)` keeps any truthy text and `join` stringifies it,
                // so a non-string text still shows up.
                Some("text") => c
                    .get("text")
                    .filter(|text| truthy(Some(text)))
                    .map(|text| js_string(Some(text))),
                Some("image") => Some("[image]".to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One CLI stdout message → zero or more ACP updates.
pub fn translate(msg: &Value, session_id: &str, state: &mut TranslateState) -> Vec<Value> {
    match type_of(Some(msg)) {
        Some("stream_event") => {
            let ev = msg.get("event");
            match type_of(ev) {
                Some("content_block_delta") => {
                    let d = ev.and_then(|ev| ev.get("delta"));
                    let (update, field) = match type_of(d) {
                        Some("text_delta") => ("agent_message_chunk", "text"),
                        Some("thinking_delta") => ("agent_thought_chunk", "thinking"),
                        _ => return Vec::new(),
                    };
                    let text = d.and_then(|d| d.get(field));
                    if !truthy(text) {
                        return Vec::new();
                    }
                    vec![note(
                        session_id,
                        json!({ "sessionUpdate": update, "content": { "type": "text", "text": text } }),
                    )]
                }
                Some("content_block_start") => {
                    let Some(b) = ev
                        .and_then(|ev| ev.get("content_block"))
                        .filter(|b| type_of(Some(b)) == Some("tool_use"))
                    else {
                        return Vec::new();
                    };
                    let name = b.get("name");
                    if name
                        .and_then(Value::as_str)
                        .is_some_and(|name| HIDDEN_TOOLS.contains(&name))
                    {
                        state.hidden.insert(id_key(b.get("id")));
                        return Vec::new();
                    }
                    vec![note(
                        session_id,
                        object([
                            ("sessionUpdate", Some(json!("tool_call"))),
                            ("toolCallId", b.get("id").cloned()),
                            ("title", name.cloned()),
                            ("status", Some(json!("pending"))),
                        ]),
                    )]
                }
                _ => Vec::new(),
            }
        }

        // Complete assistant messages repeat the streamed text; only the final tool input is new.
        Some("assistant") => content_blocks(msg)
            .iter()
            .filter(|c| type_of(Some(c)) == Some("tool_use") && !state.is_hidden(c.get("id")))
            .map(|c| {
                note(
                    session_id,
                    object([
                        ("sessionUpdate", Some(json!("tool_call_update"))),
                        ("toolCallId", c.get("id").cloned()),
                        ("rawInput", c.get("input").cloned()),
                        ("status", Some(json!("in_progress"))),
                    ]),
                )
            })
            .collect(),

        Some("user") => content_blocks(msg)
            .iter()
            .filter(|c| {
                type_of(Some(c)) == Some("tool_result") && !state.is_hidden(c.get("tool_use_id"))
            })
            .map(|c| {
                let text = result_text(c.get("content"));
                let status = if truthy(c.get("is_error")) {
                    "failed"
                } else {
                    "completed"
                };
                note(
                    session_id,
                    object([
                        ("sessionUpdate", Some(json!("tool_call_update"))),
                        ("toolCallId", c.get("tool_use_id").cloned()),
                        ("status", Some(json!(status))),
                        ("rawOutput", Some(json!(text))),
                        (
                            "content",
                            Some(json!([{ "type": "content", "content": { "type": "text", "text": text } }])),
                        ),
                    ]),
                )
            })
            .collect(),

        _ => Vec::new(),
    }
}

/// `msg.message.content` when it is an array, else nothing.
fn content_blocks(msg: &Value) -> &[Value] {
    msg.pointer("/message/content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// Serializes as `{"stopReason": …}` or `{"error": …}`, the two shapes of the TypeScript union.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnOutcome {
    StopReason(String),
    Error(String),
}

/// A `result` message → the turn's stop reason, or an error to raise.
pub fn turn_outcome(msg: &Value, interrupted: bool) -> TurnOutcome {
    if interrupted {
        return TurnOutcome::StopReason("cancelled".to_owned());
    }
    if msg.get("subtype").and_then(Value::as_str) == Some("success") {
        let stop_reason = msg
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn");
        return TurnOutcome::StopReason(stop_reason.to_owned());
    }
    let detail = match msg.get("errors").and_then(Value::as_array) {
        Some(errors) if !errors.is_empty() => js_join(errors, "; "),
        _ => js_string(msg.get("subtype")),
    };
    TurnOutcome::Error(format!("Claude Code turn failed: {detail}"))
}

fn permission_options() -> Value {
    json!([
        { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
        { "optionId": "allow_always", "name": "Always allow", "kind": "allow_always" },
        { "optionId": "reject", "name": "Reject", "kind": "reject_once" }
    ])
}

/// can_use_tool control_request → the ACP permission request the UI's queue understands.
pub fn to_permission_request(session_id: &str, req: &Value) -> Value {
    let tool_name = req.get("tool_name");
    let tool_call_id = req
        .get("tool_use_id")
        .filter(|id| !id.is_null())
        .or(tool_name);
    json!({
        "sessionId": session_id,
        // title carries the tool id, which risky-tool matching looks for.
        "toolCall": object([
            ("toolCallId", tool_call_id.cloned()),
            ("title", tool_name.cloned()),
            ("rawInput", req.get("input").cloned()),
        ]),
        "options": permission_options()
    })
}

/// ACP permission answer → the control_response the CLI expects.
pub fn permission_response(request_id: &str, answer: &Value, input: Option<&Value>) -> String {
    let allow = answer.pointer("/outcome/outcome").and_then(Value::as_str) == Some("selected")
        && answer
            .pointer("/outcome/optionId")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with("allow"));
    let response = if allow {
        let updated_input = input
            .filter(|input| !input.is_null())
            .cloned()
            .unwrap_or_else(|| json!({}));
        json!({ "behavior": "allow", "updatedInput": updated_input })
    } else {
        json!({ "behavior": "deny", "message": "The user rejected this tool call." })
    };
    line(&json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response }
    }))
}

pub fn error_response(request_id: &str, error: &str) -> String {
    line(&json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": request_id, "error": error }
    }))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliModelEntry {
    pub model_id: String,
    pub name: String,
    pub description: Option<String>,
    pub effort_levels: Vec<String>,
    pub supports_fast_mode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliModelState {
    pub available_models: Vec<CliModelEntry>,
    pub current_model_id: Option<String>,
    pub effort: Option<String>,
    pub fast_mode: bool,
}

/// initialize's model list → the model-picker state.
pub fn model_state(
    models: &[CliModel],
    current: Option<&str>,
    options: Option<&CliOptions>,
) -> CliModelState {
    CliModelState {
        available_models: models
            .iter()
            .map(|m| CliModelEntry {
                model_id: m.value.clone(),
                name: m.display_name.clone().unwrap_or_else(|| m.value.clone()),
                description: m.description.clone(),
                effort_levels: m.supported_effort_levels.clone().unwrap_or_default(),
                supports_fast_mode: m.supports_fast_mode == Some(true),
            })
            .collect(),
        current_model_id: current.map(str::to_owned),
        effort: options.and_then(|o| o.effort.clone()),
        fast_mode: options.is_some_and(|o| o.fast_mode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "claude-cli:s1";

    fn upd_with(msg: Value, state: &mut TranslateState) -> Vec<Value> {
        translate(&msg, SID, state)
            .into_iter()
            .map(|n| n["update"].clone())
            .collect()
    }

    fn upd(msg: Value) -> Vec<Value> {
        upd_with(msg, &mut TranslateState::default())
    }

    // ── claudeCliArgs ──

    #[test]
    fn 구독_인증을_지키고_권한_요청을_호스트로_보낸다() {
        let args = claude_cli_args("http://127.0.0.1:1/mcp", "Bearer t", None);
        assert!(!args.iter().any(|a| a == "--bare"));
        assert!(args.join(" ").contains("--permission-prompt-tool stdio"));
        let at = args.iter().position(|a| a == "--mcp-config").unwrap();
        let mcp: Value = serde_json::from_str(&args[at + 1]).unwrap();
        let server = &mcp["mcpServers"]["rever-traffic"];
        assert_eq!(server["type"], "http");
        assert_eq!(server["url"], "http://127.0.0.1:1/mcp");
        assert!(!args.iter().any(|a| a == "--model"));
        let with_model = claude_cli_args("u", "a", Some("opus"));
        assert_eq!(with_model[with_model.len() - 2..], ["--model", "opus"]);
    }

    #[test]
    fn 중첩_하위_에이전트_task_도구를_차단한다() {
        let args = claude_cli_args("u", "a", None);
        assert!(args.join(" ").contains("--disallowed-tools Task"));
    }

    // ── translate ──

    #[test]
    fn 텍스트_생각_델타를_메시지_생각_청크로_바꾼다() {
        assert_eq!(
            upd(
                json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "hi" } } })
            ),
            [
                json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hi" } })
            ]
        );
        let thought = upd(
            json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "thinking_delta", "thinking": "hm" } } }),
        );
        assert_eq!(thought[0]["sessionUpdate"], "agent_thought_chunk");
        assert_eq!(
            upd(
                json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "signature_delta", "signature": "x" } } })
            ),
            Vec::<Value>::new()
        );
    }

    #[test]
    fn 도구_호출을_시작_입력_결과_순서로_알린다() {
        let mut s = TranslateState::default();
        let id = "toolu_1";
        assert_eq!(
            upd_with(
                json!({ "type": "stream_event", "event": { "type": "content_block_start", "content_block": { "type": "tool_use", "id": id, "name": "mcp__rever-traffic__list_requests", "input": {} } } }),
                &mut s
            ),
            [
                json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": "mcp__rever-traffic__list_requests", "status": "pending" })
            ]
        );
        assert_eq!(
            upd_with(
                json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "dup" }, { "type": "tool_use", "id": id, "name": "x", "input": { "limit": 1 } }] } }),
                &mut s
            ),
            [
                json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "rawInput": { "limit": 1 }, "status": "in_progress" })
            ]
        );
        let done = upd_with(
            json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "tool_use_id": id, "content": [{ "type": "text", "text": "[1]" }] }] } }),
            &mut s,
        );
        assert_eq!(done[0]["sessionUpdate"], "tool_call_update");
        assert_eq!(done[0]["toolCallId"], id);
        assert_eq!(done[0]["status"], "completed");
        assert_eq!(done[0]["rawOutput"], "[1]");
    }

    #[test]
    fn 실패한_도구_결과는_failed와_에러_문구를_담는다() {
        let updates = upd(
            json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "tool_use_id": "t", "is_error": true, "content": "denied" }] } }),
        );
        let u = &updates[0];
        assert_eq!(u["status"], "failed");
        assert_eq!(
            u["content"],
            json!([{ "type": "content", "content": { "type": "text", "text": "denied" } }])
        );
    }

    #[test]
    fn toolsearch_같은_내부_도구는_숨긴다() {
        let mut s = TranslateState::default();
        let none = Vec::<Value>::new();
        assert_eq!(
            upd_with(
                json!({ "type": "stream_event", "event": { "type": "content_block_start", "content_block": { "type": "tool_use", "id": "ts", "name": "ToolSearch" } } }),
                &mut s
            ),
            none
        );
        assert_eq!(
            upd_with(
                json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "id": "ts", "name": "ToolSearch", "input": {} }] } }),
                &mut s
            ),
            none
        );
        assert_eq!(
            upd_with(
                json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "tool_use_id": "ts", "content": [] }] } }),
                &mut s
            ),
            none
        );
    }

    #[test]
    fn 시스템_결과_메시지는_업데이트를_만들지_않는다() {
        assert_eq!(
            upd(json!({ "type": "system", "subtype": "init" })),
            Vec::<Value>::new()
        );
        assert_eq!(
            upd(json!({ "type": "result", "subtype": "success" })),
            Vec::<Value>::new()
        );
    }

    // Not in the TypeScript tests: `filter(Boolean).join` keeps truthy non-string text.
    #[test]
    fn 도구_결과의_문자열이_아닌_text도_join처럼_문자열로_바꾼다() {
        let updates = upd(
            json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "tool_use_id": "t", "content": [
                { "type": "text", "text": 42 },
                { "type": "image" },
                { "type": "text", "text": "" },
                { "type": "text", "text": 0 },
                { "type": "text", "text": null },
                { "type": "text", "text": true },
                { "type": "text", "text": { "a": 1 } },
                { "type": "text", "text": ["x", null, "y"] },
                { "type": "other", "text": "skipped" },
                { "type": "text", "text": "a" }
            ] }] } }),
        );
        assert_eq!(
            updates[0]["rawOutput"],
            "42\n[image]\ntrue\n[object Object]\nx,,y\na"
        );
    }

    // ── turnOutcome ──

    #[test]
    fn 성공_취소_실패를_구분한다() {
        let outcome = |msg: Value, interrupted| {
            serde_json::to_value(turn_outcome(&msg, interrupted)).unwrap()
        };
        assert_eq!(
            outcome(
                json!({ "subtype": "success", "stop_reason": "end_turn" }),
                false
            ),
            json!({ "stopReason": "end_turn" })
        );
        assert_eq!(
            outcome(json!({ "subtype": "error_during_execution" }), true),
            json!({ "stopReason": "cancelled" })
        );
        assert_eq!(
            outcome(
                json!({ "subtype": "error_max_turns", "errors": ["too many"] }),
                false
            ),
            json!({ "error": "Claude Code turn failed: too many" })
        );
    }

    // Not in the TypeScript tests: `errors.join('; ')` and `${subtype}` on loose values.
    #[test]
    fn 실패_사유는_js의_join과_문자열_변환을_따른다() {
        assert_eq!(
            turn_outcome(
                &json!({ "subtype": "x", "errors": ["a", null, 3, { "m": 1 }, ["b", "c"]] }),
                false
            ),
            TurnOutcome::Error("Claude Code turn failed: a; ; 3; [object Object]; b,c".to_owned())
        );
        assert_eq!(
            turn_outcome(
                &json!({ "subtype": "error_max_turns", "errors": [] }),
                false
            ),
            TurnOutcome::Error("Claude Code turn failed: error_max_turns".to_owned())
        );
        assert_eq!(
            turn_outcome(&json!({}), false),
            TurnOutcome::Error("Claude Code turn failed: undefined".to_owned())
        );
        assert_eq!(
            turn_outcome(&json!({ "subtype": "success" }), false),
            TurnOutcome::StopReason("end_turn".to_owned())
        );
    }

    // ── 권한 왕복 ──

    fn can_use_tool() -> Value {
        json!({ "subtype": "can_use_tool", "tool_name": "mcp__rever-traffic__intruder_run", "input": { "n": 1 }, "tool_use_id": "toolu_9" })
    }

    #[test]
    fn can_use_tool을_위험_도구_판별이_가능한_acp_요청으로_바꾼다() {
        let r = to_permission_request(SID, &can_use_tool());
        assert!(
            r["toolCall"]["title"]
                .as_str()
                .unwrap()
                .contains("intruder_run")
        );
        assert_eq!(r["toolCall"]["rawInput"], json!({ "n": 1 }));
        let kinds: Vec<&str> = r["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["allow_once", "allow_always", "reject_once"]);
    }

    #[test]
    fn 허용은_원래_입력과_함께_allow_거절은_deny로_답한다() {
        let parse = |line: String| serde_json::from_str::<Value>(&line).unwrap();
        let allow = parse(permission_response(
            "r1",
            &json!({ "outcome": { "outcome": "selected", "optionId": "allow_always" } }),
            Some(&json!({ "n": 1 })),
        ));
        assert_eq!(
            allow["response"],
            json!({ "subtype": "success", "request_id": "r1", "response": { "behavior": "allow", "updatedInput": { "n": 1 } } })
        );
        let deny = parse(permission_response(
            "r2",
            &json!({ "outcome": { "outcome": "selected", "optionId": "reject" } }),
            Some(&json!({})),
        ));
        assert_eq!(deny["response"]["response"]["behavior"], "deny");
        let cancelled = parse(permission_response(
            "r3",
            &json!({ "outcome": { "outcome": "cancelled" } }),
            Some(&json!({})),
        ));
        assert_eq!(cancelled["response"]["response"]["behavior"], "deny");
    }

    // ── modelState ──

    #[test]
    fn initialize_모델_목록과_현재_추론_수준_빠른_모드를_모델_선택기_형식으로_바꾼다() {
        let models: Vec<CliModel> = serde_json::from_value(json!([
            { "value": "opus", "displayName": "Opus 5.5", "description": "x", "supportedEffortLevels": ["low", "high"], "supportsFastMode": true }
        ]))
        .unwrap();
        let options = CliOptions {
            effort: Some("high".to_owned()),
            fast_mode: false,
        };
        assert_eq!(
            serde_json::to_value(model_state(&models, Some("opus"), Some(&options))).unwrap(),
            json!({
                "availableModels": [{ "modelId": "opus", "name": "Opus 5.5", "description": "x", "effortLevels": ["low", "high"], "supportsFastMode": true }],
                "currentModelId": "opus",
                "effort": "high",
                "fastMode": false
            })
        );
        let haiku = CliModel {
            value: "haiku".to_owned(),
            ..CliModel::default()
        };
        let state = model_state(&[haiku], None, None);
        assert_eq!(
            state.available_models[0].effort_levels,
            Vec::<String>::new()
        );
        assert!(!state.available_models[0].supports_fast_mode);
    }

    // ── 추론 수준·빠른 모드 ──

    #[test]
    fn get_settings_응답에서_실제_적용된_값을_읽는다() {
        assert_eq!(
            options_from_settings(
                &json!({ "effective": { "fastMode": true }, "applied": { "effort": "low" } })
            ),
            CliOptions {
                effort: Some("low".to_owned()),
                fast_mode: true
            }
        );
        assert_eq!(
            options_from_settings(&json!({ "applied": { "effort": null } })),
            CliOptions {
                effort: None,
                fast_mode: false
            }
        );
    }

    #[test]
    fn apply_flag_settings_요청을_만들고_잘못된_값은_거부한다() {
        assert_eq!(
            flag_settings_request(FlagKey::Effort, &json!("xhigh")),
            Ok(json!({ "subtype": "apply_flag_settings", "settings": { "effortLevel": "xhigh" } }))
        );
        assert_eq!(
            flag_settings_request(FlagKey::FastMode, &json!(true)),
            Ok(json!({ "subtype": "apply_flag_settings", "settings": { "fastMode": true } }))
        );
        assert!(flag_settings_request(FlagKey::Effort, &json!(true)).is_err());
        assert!(flag_settings_request(FlagKey::FastMode, &json!("yes")).is_err());
    }
}
