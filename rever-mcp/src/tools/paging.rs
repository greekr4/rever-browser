//! Port of `src/main/mcp/tools/paging.ts` — `tool_result_more`: the next slice
//! of a result that `ok_budgeted` truncated.
//!
//! The parked results live in `ToolContext::state::<Mutex<ResultStore>>()`: a
//! tool that truncates its result must park it in that same store, or the
//! resultId in its footer leads nowhere.

use std::sync::{Mutex, PoisonError};

use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ResultStore, ToolResult, err, ok};
use serde_json::{Value, json};

use crate::validation::{at_least_zero, at_most, int, invalid_arguments, object, string};

pub struct ToolResultMore;

impl Tool for ToolResultMore {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "tool_result_more",
            "Fetch the next slice of a large tool result that was truncated with a \"[truncated: … resultId=…]\" footer. Pass the resultId and offset from that footer. Returns the slice plus nextOffset (null when done).",
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "resultId": {
                        "type": "string",
                        "description": "resultId from a truncated result footer"
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 9_007_199_254_740_991_i64,
                        "description": "Character offset to resume from"
                    },
                    "limit": {
                        "description": "Max chars to return (default 12000)",
                        "type": "integer",
                        "exclusiveMinimum": 0,
                        "maximum": 60000
                    }
                },
                "required": ["resultId", "offset"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let store = ctx.state::<Mutex<ResultStore>>();
            let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
            tool_result_more(&mut store, &args)
        })
    }
}

fn tool_result_more(store: &mut ResultStore, args: &Value) -> ToolResult {
    let args = match object(args) {
        Ok(args) => args,
        Err(issue) => return invalid_arguments("tool_result_more", &[issue]),
    };
    let mut issues = Vec::new();
    let result_id = string(args, "resultId", &mut issues);
    let offset = int(args.get("offset"), "offset", &mut issues);
    if let Some(offset) = offset {
        at_least_zero(offset, true, "offset", &mut issues);
    }
    // Optional: only a missing key means "default"; null is a wrong value.
    let limit = match args.get("limit") {
        None => None,
        Some(value) => {
            let limit = int(Some(value), "limit", &mut issues);
            if let Some(limit) = limit {
                at_least_zero(limit, false, "limit", &mut issues);
                at_most(limit, 60_000, "limit", &mut issues);
            }
            limit
        }
    };
    let (Some(result_id), Some(offset), true) = (result_id, offset, issues.is_empty()) else {
        return invalid_arguments("tool_result_more", &issues);
    };

    // Both passed the integer and range checks above, so the casts are exact.
    let page = store.get_result_slice(result_id, offset as usize, limit.map(|l| l as usize));
    let Some(page) = page else {
        return err(format!(
            "unknown or expired resultId: {result_id} (results are kept briefly; re-run the original tool)"
        ));
    };
    let footer = match page.next_offset {
        Some(next_offset) => format!(
            "\n\n[more: call tool_result_more resultId=\"{result_id}\", offset={next_offset} ({} chars left)]",
            page.total.saturating_sub(page.end)
        ),
        None => format!("\n\n[end of result: {} chars total]", page.total),
    };
    ok(page.slice + &footer)
}

pub fn register_paging_tools(registry: &mut ToolRegistry) {
    registry.register(ToolResultMore);
}

#[cfg(test)]
mod tests {
    use rever_tools_api::utils::Content;

    use super::*;

    fn text_of(result: &ToolResult) -> &str {
        match result.content.first() {
            Some(Content::Text { text }) => text,
            None => "",
        }
    }

    /// A store holding "0123456789" as res_1.
    fn store() -> ResultStore {
        let mut store = ResultStore::new();
        store.ok_budgeted("0123456789", Some(1));
        store
    }

    /// The issue list in the refusal, parsed back.
    fn issues(result: &ToolResult) -> Value {
        assert!(result.is_error);
        let (prefix, list) = text_of(result).split_once(": [").unwrap();
        assert_eq!(
            prefix,
            "MCP error -32602: Input validation error: Invalid arguments for tool tool_result_more"
        );
        serde_json::from_str(&format!("[{list}")).unwrap()
    }

    #[test]
    fn 남은_조각이_있으면_다음_offset과_남은_글자_수를_알린다() {
        let mut store = store();
        let result = tool_result_more(
            &mut store,
            &json!({ "resultId": "res_1", "offset": 2, "limit": 3 }),
        );
        assert_eq!(
            result,
            ok("234\n\n[more: call tool_result_more resultId=\"res_1\", offset=5 (5 chars left)]")
        );
    }

    #[test]
    fn 마지막_조각에는_전체_길이를_적는다() {
        let mut store = store();
        let result = tool_result_more(&mut store, &json!({ "resultId": "res_1", "offset": 8 }));
        assert_eq!(result, ok("89\n\n[end of result: 10 chars total]"));

        // Past the end: an empty slice, still not an error.
        let result = tool_result_more(&mut store, &json!({ "resultId": "res_1", "offset": 99 }));
        assert_eq!(result, ok("\n\n[end of result: 10 chars total]"));
    }

    #[test]
    fn limit을_생략하면_12000자씩_돌려준다() {
        let mut store = ResultStore::new();
        store.ok_budgeted("a".repeat(30_000), None);
        let result = tool_result_more(&mut store, &json!({ "resultId": "res_1", "offset": 0 }));
        assert_eq!(
            text_of(&result),
            format!(
                "{}\n\n[more: call tool_result_more resultId=\"res_1\", offset=12000 (18000 chars left)]",
                "a".repeat(12_000)
            )
        );
    }

    #[test]
    fn limit은_60000까지_받는다() {
        let mut store = ResultStore::new();
        store.ok_budgeted("a".repeat(70_000), None);
        let result = tool_result_more(
            &mut store,
            &json!({ "resultId": "res_1", "offset": 0, "limit": 60_000 }),
        );
        assert!(text_of(&result).ends_with("offset=60000 (10000 chars left)]"));

        let result = tool_result_more(
            &mut store,
            &json!({ "resultId": "res_1", "offset": 0, "limit": 60_001 }),
        );
        assert_eq!(
            issues(&result),
            json!([{
                "origin": "number", "code": "too_big", "maximum": 60000, "inclusive": true,
                "path": ["limit"], "message": "Too big: expected number to be <=60000"
            }])
        );
    }

    #[test]
    fn offset과_글자_수는_utf16_단위다() {
        let mut store = ResultStore::new();
        store.ok_budgeted("가나😀다라", Some(1));
        let result = tool_result_more(
            &mut store,
            &json!({ "resultId": "res_1", "offset": 2, "limit": 3 }),
        );
        assert_eq!(
            result,
            ok("😀다\n\n[more: call tool_result_more resultId=\"res_1\", offset=5 (1 chars left)]")
        );
    }

    #[test]
    fn 모르는_result_id는_다시_실행하라고_알린다() {
        let mut store = store();
        let result = tool_result_more(&mut store, &json!({ "resultId": "res_9", "offset": 0 }));
        assert_eq!(
            result,
            err(
                "unknown or expired resultId: res_9 (results are kept briefly; re-run the original tool)"
            )
        );
    }

    #[test]
    fn 정수로_쓴_실수는_정수로_받는다() {
        let mut store = store();
        let args =
            serde_json::from_str(r#"{"resultId":"res_1","offset":8.0,"limit":1e1}"#).unwrap();
        assert_eq!(
            tool_result_more(&mut store, &args),
            ok("89\n\n[end of result: 10 chars total]")
        );
    }

    #[test]
    fn 빠진_필수_인자는_sdk와_같은_글로_거절한다() {
        let mut store = store();
        // Byte for byte what the TypeScript SDK returned for `{}` (zod 4).
        assert_eq!(
            tool_result_more(&mut store, &json!({})),
            err(
                "MCP error -32602: Input validation error: Invalid arguments for tool tool_result_more: [\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"resultId\"\n    ],\n    \"message\": \"Invalid input: expected string, received undefined\"\n  },\n  {\n    \"expected\": \"number\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"offset\"\n    ],\n    \"message\": \"Invalid input: expected number, received undefined\"\n  }\n]"
            )
        );
    }

    #[test]
    fn 인자가_객체가_아니면_거절한다() {
        let mut store = store();
        assert_eq!(
            issues(&tool_result_more(&mut store, &Value::Null)),
            json!([{
                "expected": "object", "code": "invalid_type", "path": [],
                "message": "Invalid input: expected object, received undefined"
            }])
        );
        assert_eq!(
            issues(&tool_result_more(&mut store, &json!([1]))),
            json!([{
                "expected": "object", "code": "invalid_type", "path": [],
                "message": "Invalid input: expected object, received array"
            }])
        );
    }

    #[test]
    fn 타입이_틀린_인자는_받은_타입을_적어_거절한다() {
        let mut store = store();
        let result = tool_result_more(&mut store, &json!({ "resultId": [], "offset": {} }));
        assert_eq!(
            issues(&result),
            json!([
                {
                    "expected": "string", "code": "invalid_type", "path": ["resultId"],
                    "message": "Invalid input: expected string, received array"
                },
                {
                    "expected": "number", "code": "invalid_type", "path": ["offset"],
                    "message": "Invalid input: expected number, received object"
                }
            ])
        );

        let cases = [
            (json!("1"), "string"),
            (json!(true), "boolean"),
            (json!(null), "null"),
        ];
        for (offset, received) in cases {
            let result = tool_result_more(
                &mut store,
                &json!({ "resultId": "res_1", "offset": offset }),
            );
            assert_eq!(
                issues(&result),
                json!([{
                    "expected": "number", "code": "invalid_type", "path": ["offset"],
                    "message": format!("Invalid input: expected number, received {received}")
                }])
            );
        }

        // limit is optional, but null is not "omitted".
        let result = tool_result_more(
            &mut store,
            &json!({ "resultId": "res_1", "offset": 0, "limit": null }),
        );
        assert_eq!(
            issues(&result),
            json!([{
                "expected": "number", "code": "invalid_type", "path": ["limit"],
                "message": "Invalid input: expected number, received null"
            }])
        );
    }

    #[test]
    fn 소수는_정수가_아니라고만_거절하고_범위는_따지지_않는다() {
        let mut store = store();
        let int_issue = |key: &str| {
            json!({
                "expected": "int", "format": "safeint", "code": "invalid_type", "path": [key],
                "message": "Invalid input: expected int, received number"
            })
        };
        let result = tool_result_more(&mut store, &json!({ "resultId": "res_1", "offset": 1.5 }));
        assert_eq!(issues(&result), json!([int_issue("offset")]));

        // -2.5 is also below the minimum, but zod stops at the first failure.
        let result = tool_result_more(
            &mut store,
            &json!({ "resultId": "res_1", "offset": 0, "limit": -2.5 }),
        );
        assert_eq!(issues(&result), json!([int_issue("limit")]));
    }

    #[test]
    fn 범위를_벗어난_정수는_경계를_적어_거절한다() {
        let mut store = store();
        let result = tool_result_more(&mut store, &json!({ "resultId": "res_1", "offset": -1 }));
        assert_eq!(
            issues(&result),
            json!([{
                "origin": "number", "code": "too_small", "minimum": 0, "inclusive": true,
                "path": ["offset"], "message": "Too small: expected number to be >=0"
            }])
        );

        let result = tool_result_more(
            &mut store,
            &json!({ "resultId": "res_1", "offset": 0, "limit": 0 }),
        );
        assert_eq!(
            issues(&result),
            json!([{
                "origin": "number", "code": "too_small", "minimum": 0, "inclusive": false,
                "path": ["limit"], "message": "Too small: expected number to be >0"
            }])
        );
    }

    #[test]
    fn 안전한_정수_범위를_넘으면_그_오류와_범위_오류를_함께_낸다() {
        let mut store = store();
        let too_big = json!({
            "code": "too_big", "maximum": 9_007_199_254_740_991_i64,
            "note": "Integers must be within the safe integer range.",
            "origin": "int", "inclusive": true, "path": ["limit"],
            "message": "Too big: expected int to be <=9007199254740991"
        });
        let result = tool_result_more(
            &mut store,
            &json!({ "resultId": "res_1", "offset": 0, "limit": 1e20 }),
        );
        assert_eq!(
            issues(&result),
            json!([too_big, {
                "origin": "number", "code": "too_big", "maximum": 60000, "inclusive": true,
                "path": ["limit"], "message": "Too big: expected number to be <=60000"
            }])
        );

        let result = tool_result_more(&mut store, &json!({ "resultId": "res_1", "offset": -1e20 }));
        assert_eq!(
            issues(&result),
            json!([
                {
                    "code": "too_small", "minimum": -9_007_199_254_740_991_i64,
                    "note": "Integers must be within the safe integer range.",
                    "origin": "int", "inclusive": true, "path": ["offset"],
                    "message": "Too small: expected int to be >=-9007199254740991"
                },
                {
                    "origin": "number", "code": "too_small", "minimum": 0, "inclusive": true,
                    "path": ["offset"], "message": "Too small: expected number to be >=0"
                }
            ])
        );

        // 2^53 is one past the safe range; 2^53 - 1 is the last offset accepted.
        let result = tool_result_more(
            &mut store,
            &json!({ "resultId": "res_1", "offset": 9_007_199_254_740_992_u64 }),
        );
        assert!(result.is_error);
        let result = tool_result_more(
            &mut store,
            &json!({ "resultId": "res_1", "offset": 9_007_199_254_740_991_u64 }),
        );
        assert_eq!(result, ok("\n\n[end of result: 10 chars total]"));
    }

    #[test]
    fn 검증에_걸린_호출은_저장소를_건드리지_않는다() {
        let mut store = ResultStore::new();
        store.ok_budgeted("ab", Some(1));
        store.ok_budgeted("cd", Some(1));
        // A refused call must not refresh res_1's recency.
        tool_result_more(&mut store, &json!({ "resultId": "res_1", "offset": -1 }));
        for _ in 0..49 {
            store.ok_budgeted("ef", Some(1));
        }
        assert!(store.get_result_slice("res_1", 0, None).is_none());
        assert!(store.get_result_slice("res_2", 0, None).is_some());
    }

    #[test]
    fn 정의는_이름과_설명과_스키마를_그대로_싣는다() {
        let def = ToolResultMore.def();
        assert_eq!(def.name, "tool_result_more");
        assert_eq!(
            def.description,
            "Fetch the next slice of a large tool result that was truncated with a \"[truncated: … resultId=…]\" footer. Pass the resultId and offset from that footer. Returns the slice plus nextOffset (null when done)."
        );
        // What the TypeScript SDK listed for the zod shape (printed by node).
        assert_eq!(
            def.input_schema.to_string(),
            r##"{"$schema":"http://json-schema.org/draft-07/schema#","type":"object","properties":{"resultId":{"type":"string","description":"resultId from a truncated result footer"},"offset":{"type":"integer","minimum":0,"maximum":9007199254740991,"description":"Character offset to resume from"},"limit":{"description":"Max chars to return (default 12000)","type":"integer","exclusiveMinimum":0,"maximum":60000}},"required":["resultId","offset"]}"##
        );

        let mut registry = ToolRegistry::default();
        register_paging_tools(&mut registry);
        assert!(registry.find("tool_result_more").is_some());
    }

    #[test]
    fn 도구는_컨텍스트의_저장소에서_읽는다() {
        let context = ToolContext::for_tests();
        context
            .state::<Mutex<ResultStore>>()
            .lock()
            .unwrap()
            .ok_budgeted("0123456789", Some(4));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let result = runtime
            .block_on(ToolResultMore.call(&context, json!({ "resultId": "res_1", "offset": 4 })));
        assert_eq!(result, ok("456789\n\n[end of result: 10 chars total]"));
    }
}
