//! Port of `src/renderer/src/ai/acp-background.ts` — frames an autonomous background turn.
//!
//! Claude Code runs a turn on its own when a background task (a Bash with
//! run_in_background, a background subagent) finishes after the user's turn
//! ended. Newer claude-agent-acp streams that turn as ordinary session updates
//! with no prompt in flight; these two predicates frame it for the chat.
//!
//! An update is the ACP `SessionUpdate` JSON object, as rever-agent emits it.

use serde_json::Value;

fn kind(update: &Value) -> Option<&str> {
    update.get("sessionUpdate").and_then(Value::as_str)
}

/// Output the user should see. Idle bookkeeping (command lists, usage, a late
/// status refresh of an earlier turn's tool call) must not open a chat turn.
pub fn starts_background_turn(update: &Value) -> bool {
    match kind(update) {
        Some("agent_message_chunk") => {
            // A chunk without a content object is malformed (the TypeScript would
            // throw on it); it is not output, so it opens nothing.
            let Some(content) = update.get("content").filter(|c| c.is_object()) else {
                return false;
            };
            content.get("type").and_then(Value::as_str) != Some("text")
                || content
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.is_empty())
        }
        Some("agent_thought_chunk" | "tool_call") => true,
        _ => false,
    }
}

/// The agent reports a turn's result as a usage_update carrying its cost;
/// mid-turn context-size updates carry none.
pub fn ends_background_turn(update: &Value) -> bool {
    kind(update) == Some("usage_update") && update.get("cost").is_some_and(|cost| !cost.is_null())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn 에이전트_텍스트_생각_도구_호출이면_자율_턴의_시작으로_본다() {
        assert!(starts_background_turn(
            &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "done" } })
        ));
        assert!(starts_background_turn(
            &json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "hm" } })
        ));
        assert!(starts_background_turn(
            &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read" })
        ));
    }

    #[test]
    fn 빈_텍스트_청크는_턴을_열지_않는다() {
        assert!(!starts_background_turn(
            &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "" } })
        ));
    }

    #[test]
    fn 명령_목록_사용량_이전_도구의_상태_갱신은_턴을_열지_않는다() {
        assert!(!starts_background_turn(
            &json!({ "sessionUpdate": "available_commands_update", "availableCommands": [] })
        ));
        assert!(!starts_background_turn(
            &json!({ "sessionUpdate": "usage_update", "used": 1, "size": 2 })
        ));
        assert!(!starts_background_turn(
            &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "old", "status": "completed" })
        ));
        assert!(!starts_background_turn(
            &json!({ "sessionUpdate": "rever_pulse" })
        ));
    }

    #[test]
    fn 비용이_붙은_usage_update_턴_결과면_턴이_끝난_것으로_본다() {
        assert!(ends_background_turn(&json!({
            "sessionUpdate": "usage_update",
            "used": 10,
            "size": 200,
            "cost": { "amount": 0.1, "currency": "USD" }
        })));
    }

    #[test]
    fn 턴_도중의_usage_update_비용_없음는_끝으로_보지_않는다() {
        assert!(!ends_background_turn(
            &json!({ "sessionUpdate": "usage_update", "used": 10, "size": 200 })
        ));
        assert!(!ends_background_turn(
            &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "x" } })
        ));
    }

    // Not in the TypeScript test file.
    #[test]
    fn 텍스트가_아닌_콘텐츠는_턴을_열고_깨진_청크와_null_비용은_무시한다() {
        assert!(starts_background_turn(
            &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "image", "data": "" } })
        ));
        assert!(!starts_background_turn(
            &json!({ "sessionUpdate": "agent_message_chunk" })
        ));
        assert!(!starts_background_turn(&json!(null)));
        assert!(!ends_background_turn(
            &json!({ "sessionUpdate": "usage_update", "cost": null })
        ));
    }
}
