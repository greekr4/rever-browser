//! Port of `src/renderer/src/ai/acp-map-update.ts` — turns ACP session updates into UI message chunks.
//!
//! An update is the ACP `SessionUpdate` JSON object as rever-agent emits it, read
//! field by field like the TypeScript did; the chunks are the `ai` package's
//! `UIMessageChunk` shapes this file produces, with the same JSON field names.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::is_js_whitespace;
use crate::provider_notice::detect_safeguard_notice;

/// Streamed turn state threaded through [`map_update`] (mutated in place) so
/// consecutive deltas extend open parts instead of each opening its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnState {
    pub text_started: bool,
    /// A reasoning run is open between a start and its close.
    pub reasoning_started: bool,
    /// How many reasoning runs opened so far this turn; ids must be unique.
    pub reasoning_runs: u32,
}

pub fn new_turn_state() -> TurnState {
    TurnState::default()
}

/// The subset of the `ai` package's `UIMessageChunk` union that the mapping emits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum UiMessageChunk {
    TextStart {
        id: String,
    },
    TextDelta {
        id: String,
        delta: String,
    },
    ReasoningStart {
        id: String,
    },
    ReasoningDelta {
        id: String,
        delta: String,
    },
    ReasoningEnd {
        id: String,
    },
    ToolInputStart {
        tool_call_id: String,
        tool_name: String,
        provider_executed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    ToolInputAvailable {
        tool_call_id: String,
        tool_name: String,
        input: Value,
        provider_executed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    ToolOutputAvailable {
        tool_call_id: String,
        /// Absent when the update carried neither a raw output nor text content.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<Value>,
        provider_executed: bool,
    },
    ToolOutputError {
        tool_call_id: String,
        error_text: String,
        provider_executed: bool,
    },
}

fn run_id(text_id: &str, state: &TurnState) -> String {
    format!("reasoning-{text_id}-{}", state.reasoning_runs)
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// JavaScript truthiness of a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `seen_tool_call_ids` is the TypeScript `options.seenToolCallIds`: without it
/// every tool_call_update is treated as the first sight of its tool call.
pub fn map_update(
    update: &Value,
    text_id: &str,
    state: &mut TurnState,
    mut seen_tool_call_ids: Option<&mut HashSet<String>>,
) -> Vec<UiMessageChunk> {
    let mut chunks = Vec::new();

    // GLM/Grok stream reasoning token-by-token: a part per delta would explode
    // into dozens of "thoughts" blocks. One part per run of consecutive deltas.
    let close_reasoning = |state: &mut TurnState, chunks: &mut Vec<UiMessageChunk>| {
        if !state.reasoning_started {
            return;
        }
        state.reasoning_started = false;
        chunks.push(UiMessageChunk::ReasoningEnd {
            id: run_id(text_id, state),
        });
    };

    let content_type = update
        .get("content")
        .and_then(|content| str_field(content, "type"));
    let content_text = update
        .get("content")
        .and_then(|content| str_field(content, "text"));
    let tool_call_id = || {
        str_field(update, "toolCallId")
            .unwrap_or_default()
            .to_string()
    };
    let title = || str_field(update, "title").map(str::to_string);
    let tool_name = || {
        str_field(update, "title")
            .filter(|title| !title.is_empty())
            .unwrap_or("unknown")
            .to_string()
    };
    let input_available = |input: &Value| UiMessageChunk::ToolInputAvailable {
        tool_call_id: tool_call_id(),
        tool_name: tool_name(),
        input: input.clone(),
        provider_executed: true,
        title: title(),
    };

    match str_field(update, "sessionUpdate") {
        Some("agent_message_chunk") => {
            close_reasoning(state, &mut chunks);
            // Non-text content is not handled; the TypeScript only logged a warning.
            if let Some(text) =
                content_text.filter(|text| content_type == Some("text") && !text.is_empty())
            {
                if !state.text_started {
                    chunks.push(UiMessageChunk::TextStart {
                        id: text_id.to_string(),
                    });
                    state.text_started = true;
                }
                // The SDK streams its safeguard / model-fallback notice glued to the
                // adjacent answer (no surrounding newlines), so it renders as one run-on
                // paragraph. Isolate a notice chunk onto its own paragraph.
                // shortcut: only catches a notice that arrives as one chunk; if the SDK
                // splits it across deltas, upgrade to buffering in the transport.
                let delta = if detect_safeguard_notice(Some(text)) {
                    format!("\n\n{}\n\n", text.trim_matches(is_js_whitespace))
                } else {
                    text.to_string()
                };
                chunks.push(UiMessageChunk::TextDelta {
                    id: text_id.to_string(),
                    delta,
                });
            }
        }
        Some("agent_thought_chunk") => {
            if content_type == Some("text") {
                if !state.reasoning_started {
                    state.reasoning_started = true;
                    state.reasoning_runs += 1;
                    chunks.push(UiMessageChunk::ReasoningStart {
                        id: run_id(text_id, state),
                    });
                }
                chunks.push(UiMessageChunk::ReasoningDelta {
                    id: run_id(text_id, state),
                    delta: content_text.unwrap_or_default().to_string(),
                });
            }
        }
        Some("tool_call") => {
            close_reasoning(state, &mut chunks);
            if let Some(seen) = seen_tool_call_ids.as_deref_mut() {
                seen.insert(tool_call_id());
            }
            chunks.push(UiMessageChunk::ToolInputStart {
                tool_call_id: tool_call_id(),
                tool_name: tool_name(),
                provider_executed: true,
                title: title(),
            });
            if let Some(input) = update.get("rawInput").filter(|input| truthy(input)) {
                chunks.push(input_available(input));
            }
        }
        Some("tool_call_update") => {
            close_reasoning(state, &mut chunks);
            // `typeof rawInput !== 'undefined'`: a present null still counts.
            let raw_input = update.get("rawInput");
            let seen = match seen_tool_call_ids {
                Some(seen) => !seen.insert(tool_call_id()),
                None => false,
            };
            if !seen {
                chunks.push(UiMessageChunk::ToolInputStart {
                    tool_call_id: tool_call_id(),
                    tool_name: tool_name(),
                    provider_executed: true,
                    title: title(),
                });
                if let Some(input) = raw_input {
                    chunks.push(input_available(input));
                }
            }

            let output_text = || text_from_content(update.get("content"));
            match str_field(update, "status") {
                Some("completed") => chunks.push(UiMessageChunk::ToolOutputAvailable {
                    tool_call_id: tool_call_id(),
                    output: update
                        .get("rawOutput")
                        .filter(|output| !output.is_null())
                        .cloned()
                        .or_else(|| output_text().map(Value::String)),
                    provider_executed: true,
                }),
                Some("failed") => chunks.push(UiMessageChunk::ToolOutputError {
                    tool_call_id: tool_call_id(),
                    error_text: output_text().unwrap_or_else(|| "Tool call failed".to_string()),
                    provider_executed: true,
                }),
                _ => {
                    if let Some(input) = raw_input {
                        chunks.push(input_available(input));
                    }
                }
            }
        }
        // rever_pulse and any other activity notice: keep the turn alive, show
        // nothing. Deliberately does NOT close an open reasoning part — pulses
        // interleave with thought deltas on quiet turns.
        _ => {}
    }

    chunks
}

/// Close the turn's open reasoning part, if any (call at turn end).
pub fn end_open_reasoning(text_id: &str, state: &mut TurnState) -> Vec<UiMessageChunk> {
    if !state.reasoning_started {
        return Vec::new();
    }
    state.reasoning_started = false;
    vec![UiMessageChunk::ReasoningEnd {
        id: run_id(text_id, state),
    }]
}

/// The text parts of a tool call's `content` array, joined by newlines.
pub fn text_from_content(content: Option<&Value>) -> Option<String> {
    let parts: Vec<&str> = content?
        .as_array()?
        .iter()
        .filter(|c| str_field(c, "type") == Some("content"))
        .filter_map(|c| c.get("content"))
        .filter(|inner| str_field(inner, "type") == Some("text"))
        .filter_map(|inner| str_field(inner, "text"))
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn thought(text: &str) -> Value {
        json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": text } })
    }

    fn tool_call() -> Value {
        json!({ "sessionUpdate": "tool_call", "toolCallId": "tc1", "title": "list_requests" })
    }

    fn types(chunks: &[UiMessageChunk]) -> Vec<String> {
        chunks
            .iter()
            .map(|chunk| {
                serde_json::to_value(chunk).unwrap_or_default()["type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn 연속되는_thought_델타는_하나의_reasoning_부품으로_합친다() {
        let mut state = new_turn_state();
        let first = map_update(&thought("먼저 "), "text-1", &mut state, None);
        let second = map_update(&thought("카탈로그를"), "text-1", &mut state, None);

        assert_eq!(types(&first), ["reasoning-start", "reasoning-delta"]);
        assert_eq!(types(&second), ["reasoning-delta"]);
        assert!(state.reasoning_started);
        assert_eq!(state.reasoning_runs, 1);
    }

    #[test]
    fn thought_뒤_tool_call이_오면_열려_있던_reasoning_부품을_닫는다() {
        let mut state = new_turn_state();
        map_update(&thought("생각 중"), "text-1", &mut state, None);
        let chunks = map_update(&tool_call(), "text-1", &mut state, None);

        assert!(matches!(
            chunks.first(),
            Some(UiMessageChunk::ReasoningEnd { .. })
        ));
        assert!(!state.reasoning_started);
    }

    #[test]
    fn tool_call_뒤_새_thought는_새_run_id로_부품을_연다() {
        let mut state = new_turn_state();
        map_update(&thought("첫 생각"), "text-1", &mut state, None);
        map_update(&tool_call(), "text-1", &mut state, None);
        let reopened = map_update(&thought("다음 생각"), "text-1", &mut state, None);

        let start = reopened
            .iter()
            .find(|c| matches!(c, UiMessageChunk::ReasoningStart { .. }));
        assert_eq!(
            start,
            Some(&UiMessageChunk::ReasoningStart {
                id: "reasoning-text-1-2".to_string()
            })
        );
        assert_eq!(state.reasoning_runs, 2);
    }

    #[test]
    fn end_open_reasoning은_열려_있는_부품만_닫는다() {
        let mut state = new_turn_state();
        assert_eq!(end_open_reasoning("text-1", &mut state), []);

        map_update(&thought("생각"), "text-1", &mut state, None);
        assert_eq!(
            serde_json::to_value(end_open_reasoning("text-1", &mut state)).unwrap_or_default(),
            json!([{ "type": "reasoning-end", "id": "reasoning-text-1-1" }])
        );
        assert_eq!(end_open_reasoning("text-1", &mut state), []);
    }

    // The cases below are not in the TypeScript test file: they cover the text,
    // tool and JSON-shape branches the reasoning tests leave out.

    fn message(text: &str) -> Value {
        json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } })
    }

    fn as_json(chunks: Vec<UiMessageChunk>) -> Value {
        serde_json::to_value(chunks).unwrap_or_default()
    }

    #[test]
    fn 텍스트_청크는_text_start를_한_번만_열고_빈_텍스트는_버린다() {
        let mut state = new_turn_state();
        assert_eq!(map_update(&message(""), "t", &mut state, None), []);
        assert!(!state.text_started);
        assert_eq!(
            as_json(map_update(&message("안녕"), "t", &mut state, None)),
            json!([
                { "type": "text-start", "id": "t" },
                { "type": "text-delta", "id": "t", "delta": "안녕" }
            ])
        );
        assert_eq!(
            types(&map_update(&message(" 하세요"), "t", &mut state, None)),
            ["text-delta"]
        );
    }

    #[test]
    fn 텍스트_청크는_열린_reasoning을_닫고_텍스트가_아닌_콘텐츠는_닫기만_한다() {
        let mut state = new_turn_state();
        map_update(&thought("생각"), "t", &mut state, None);
        let image = json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "image", "data": "x" } });
        assert_eq!(
            types(&map_update(&image, "t", &mut state, None)),
            ["reasoning-end"]
        );
        assert!(!state.text_started);
    }

    #[test]
    fn 세이프가드_노티스_청크는_앞뒤_빈_줄로_떼어_낸다() {
        let mut state = new_turn_state();
        let chunks = map_update(
            &message("\u{FEFF} Opus 4.8's safeguards flagged this message.\n"),
            "t",
            &mut state,
            None,
        );
        assert_eq!(
            chunks.last(),
            Some(&UiMessageChunk::TextDelta {
                id: "t".to_string(),
                delta: "\n\nOpus 4.8's safeguards flagged this message.\n\n".to_string()
            })
        );
    }

    #[test]
    fn pulse_같은_알림은_아무것도_내지_않고_열린_reasoning도_닫지_않는다() {
        let mut state = new_turn_state();
        map_update(&thought("생각"), "t", &mut state, None);
        for update in [
            json!({ "sessionUpdate": "rever_pulse" }),
            json!({ "sessionUpdate": "usage_update", "used": 1 }),
            json!({}),
            json!(null),
        ] {
            assert_eq!(map_update(&update, "t", &mut state, None), []);
        }
        assert!(state.reasoning_started);
    }

    #[test]
    fn tool_call은_입력이_truthy일_때만_tool_input_available을_낸다() {
        let mut state = new_turn_state();
        let mut seen = HashSet::new();
        assert_eq!(
            as_json(map_update(&tool_call(), "t", &mut state, Some(&mut seen))),
            json!([{
                "type": "tool-input-start",
                "toolCallId": "tc1",
                "toolName": "list_requests",
                "providerExecuted": true,
                "title": "list_requests"
            }])
        );
        assert!(seen.contains("tc1"));

        let with_input = json!({ "sessionUpdate": "tool_call", "toolCallId": "tc2", "rawInput": { "limit": 1 } });
        assert_eq!(
            as_json(map_update(&with_input, "t", &mut state, None)),
            json!([
                { "type": "tool-input-start", "toolCallId": "tc2", "toolName": "unknown", "providerExecuted": true },
                {
                    "type": "tool-input-available",
                    "toolCallId": "tc2",
                    "toolName": "unknown",
                    "input": { "limit": 1 },
                    "providerExecuted": true
                }
            ])
        );

        for falsy in [json!(null), json!(0), json!(""), json!(false)] {
            let update = json!({ "sessionUpdate": "tool_call", "toolCallId": "tc3", "title": "x", "rawInput": falsy });
            assert_eq!(
                types(&map_update(&update, "t", &mut state, None)),
                ["tool-input-start"]
            );
        }
    }

    #[test]
    fn 처음_보는_tool_call_update는_시작_청크를_만들고_본_적_있으면_입력만_갱신한다() {
        let mut state = new_turn_state();
        let mut seen = HashSet::new();
        let update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "tc1",
            "rawInput": { "limit": 1 },
            "status": "in_progress"
        });
        // The first sight emits the input twice, exactly as the TypeScript does.
        assert_eq!(
            types(&map_update(&update, "t", &mut state, Some(&mut seen))),
            [
                "tool-input-start",
                "tool-input-available",
                "tool-input-available"
            ]
        );
        assert_eq!(
            types(&map_update(&update, "t", &mut state, Some(&mut seen))),
            ["tool-input-available"]
        );
        // Without the set nothing is remembered.
        assert_eq!(types(&map_update(&update, "t", &mut state, None)).len(), 3);

        // A known call after tool_call: a status-only update emits nothing.
        map_update(&tool_call(), "t", &mut state, Some(&mut seen));
        let pending = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "tc1", "status": "pending" });
        assert_eq!(map_update(&pending, "t", &mut state, Some(&mut seen)), []);
        // A present null input is still "defined".
        let null_input =
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "tc1", "rawInput": null });
        assert_eq!(
            as_json(map_update(&null_input, "t", &mut state, Some(&mut seen))),
            json!([{
                "type": "tool-input-available",
                "toolCallId": "tc1",
                "toolName": "unknown",
                "input": null,
                "providerExecuted": true
            }])
        );
    }

    #[test]
    fn 완료와_실패는_출력_청크로_바꾼다() {
        let mut state = new_turn_state();
        let mut seen = HashSet::from(["tc1".to_string()]);
        let content = json!([
            { "type": "content", "content": { "type": "text", "text": "첫 줄" } },
            { "type": "diff", "path": "a" },
            { "type": "content", "content": { "type": "image" } },
            { "type": "content", "content": { "type": "text", "text": "둘째 줄" } }
        ]);
        let mut done = |fields: Value| {
            let mut update = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "tc1" });
            if let (Some(update), Some(fields)) = (update.as_object_mut(), fields.as_object()) {
                update.extend(fields.clone());
            }
            as_json(map_update(&update, "t", &mut state, Some(&mut seen)))
        };

        assert_eq!(
            done(json!({ "status": "completed", "rawOutput": { "ok": true }, "content": content })),
            json!([{
                "type": "tool-output-available",
                "toolCallId": "tc1",
                "output": { "ok": true },
                "providerExecuted": true
            }])
        );
        // A null raw output falls back to the text content.
        assert_eq!(
            done(json!({ "status": "completed", "rawOutput": null, "content": content }))[0]["output"],
            "첫 줄\n둘째 줄"
        );
        assert_eq!(
            done(json!({ "status": "completed" })),
            json!([{ "type": "tool-output-available", "toolCallId": "tc1", "providerExecuted": true }])
        );
        assert_eq!(
            done(json!({ "status": "failed", "content": content })),
            json!([{
                "type": "tool-output-error",
                "toolCallId": "tc1",
                "errorText": "첫 줄\n둘째 줄",
                "providerExecuted": true
            }])
        );
        assert_eq!(
            done(json!({ "status": "failed", "content": null }))[0]["errorText"],
            "Tool call failed"
        );
    }

    #[test]
    fn text_from_content는_텍스트_부품이_없으면_none이다() {
        assert_eq!(text_from_content(None), None);
        assert_eq!(text_from_content(Some(&json!([]))), None);
        assert_eq!(text_from_content(Some(&json!("text"))), None);
        assert_eq!(
            text_from_content(Some(
                &json!([null, { "type": "content" }, { "type": "content", "content": { "type": "text", "text": 1 } }])
            )),
            None
        );
        assert_eq!(
            text_from_content(Some(
                &json!([{ "type": "content", "content": { "type": "text", "text": "" } }])
            )),
            Some(String::new())
        );
    }

    // Checked against the TypeScript with bun: `??` keeps a falsy raw output, a
    // failed call ignores the raw output, and an empty title stays in the chunk.
    #[test]
    fn 거짓_같은_출력과_빈_제목_빈_생각도_타입스크립트와_같게_낸다() {
        let mut state = new_turn_state();
        let mut seen = HashSet::from(["a".to_string()]);
        let mut run =
            |update: Value| as_json(map_update(&update, "t", &mut state, Some(&mut seen)));
        let text = json!([{ "type": "content", "content": { "type": "text", "text": "L1" } }]);

        for output in [json!(false), json!(0), json!("")] {
            assert_eq!(
                run(json!({
                    "sessionUpdate": "tool_call_update", "toolCallId": "a", "status": "completed",
                    "rawOutput": output, "content": text
                })),
                json!([{ "type": "tool-output-available", "toolCallId": "a", "output": output, "providerExecuted": true }])
            );
        }
        assert_eq!(
            run(
                json!({ "sessionUpdate": "tool_call_update", "toolCallId": "a", "status": "failed", "rawOutput": "ignored" })
            )[0]["errorText"],
            "Tool call failed"
        );
        // An empty text part is still text: `??` does not fall back on "".
        assert_eq!(
            run(json!({
                "sessionUpdate": "tool_call_update", "toolCallId": "a", "status": "failed",
                "content": [{ "type": "content", "content": { "type": "text", "text": "" } }]
            }))[0]["errorText"],
            ""
        );
        assert_eq!(
            run(json!({ "sessionUpdate": "tool_call", "toolCallId": "b", "title": "" })),
            json!([{ "type": "tool-input-start", "toolCallId": "b", "toolName": "unknown", "providerExecuted": true, "title": "" }])
        );
        assert_eq!(
            run(thought("")),
            json!([
                { "type": "reasoning-start", "id": "reasoning-t-1" },
                { "type": "reasoning-delta", "id": "reasoning-t-1", "delta": "" }
            ])
        );
    }
}
