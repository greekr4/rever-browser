//! Port of `src/main/mcp/tools/console.ts` — console_eval, console_logs,
//! console_exceptions, console_clear and console_get_props.
//!
//! The TypeScript evaluates through CDP and keeps the returned remote objects
//! in a handle table (`@hN` → objectId) for console_get_props. `Browser::evaluate`
//! returns the value itself, as JSON, and never an object id: console_eval
//! prints that value, no handle is ever issued, and so there is no handle
//! table to keep and console_get_props has nothing it could look up.

use rever_tools_api::tool::{ToolContext, ToolDef, unsupported};
use rever_tools_api::utils::{ToolResult, ok};
use serde_json::json;

use crate::args::{Args, int_prop, limit_prop, no_input, prop, schema};
use crate::js::json_stringify;
use crate::{ok_pretty, store};

tool!(
    ConsoleEval,
    ToolDef::new(
        "console_eval",
        "Evaluate a JavaScript expression in the active page. Supports await. Returns a preview of the result; complex objects get a short handle (@hN) for further inspection with console_get_props.",
        schema(
            json!({
                "expression": { "type": "string", "description": "JavaScript expression to evaluate" },
                "contextId": int_prop("Optional execution context ID")
            }),
            &["expression"],
        ),
    ),
    |ctx, args| console_eval(ctx, &args).await
);

async fn console_eval(ctx: &ToolContext, args: &Args<'_>) -> Result<ToolResult, String> {
    let expression = args.required_str("expression")?;
    if args.any_int("contextId")?.is_some() {
        return Err(unsupported("evaluating in a chosen execution context"));
    }
    // A thrown exception arrives as the error, like exceptionDetails does.
    let value = ctx.browser.evaluate(expression).await?;
    // formatRemoteObject's by-value branch, JSON.stringify(value): the only
    // one a value without a remote object can take.
    Ok(ok(json_stringify(&value)))
}

tool!(
    ConsoleLogs,
    ToolDef::new(
        "console_logs",
        "Return captured console.log/warn/error messages from the active page.",
        schema(
            json!({
                "since": prop("number", "Only include logs after this epoch ms"),
                "limit": limit_prop("Max items (default 100)", 500)
            }),
            &[],
        ),
    ),
    |ctx, args| console_logs(ctx, &args)
);

fn console_logs(ctx: &ToolContext, args: &Args) -> Result<ToolResult, String> {
    let since = args.since()?;
    let limit = args.limit(500)?.unwrap_or(100);
    let store = store(ctx);
    let logs = store.console_since(since);
    ok_pretty(
        &logs
            .get(logs.len().saturating_sub(limit)..)
            .unwrap_or_default(),
    )
}

tool!(
    ConsoleExceptions,
    ToolDef::new(
        "console_exceptions",
        "Return captured JavaScript runtime exceptions from the active page.",
        schema(
            json!({ "limit": limit_prop("Max items (default 50)", 200) }),
            &[],
        ),
    ),
    |ctx, args| console_exceptions(ctx, &args)
);

fn console_exceptions(ctx: &ToolContext, args: &Args) -> Result<ToolResult, String> {
    let limit = args.limit(200)?.unwrap_or(50);
    let store = store(ctx);
    let exceptions = store.exceptions();
    ok_pretty(
        &exceptions
            .get(exceptions.len().saturating_sub(limit)..)
            .unwrap_or_default(),
    )
}

tool!(
    ConsoleClear,
    ToolDef::new(
        "console_clear",
        "Clear the captured console log buffer and all object handles from previous console_eval calls.",
        no_input(),
    ),
    |ctx, _args| {
        store(ctx).clear_console();
        Ok(ok("console logs cleared and handles reset"))
    }
);

tool!(
    ConsoleGetProps,
    ToolDef::new(
        "console_get_props",
        "Inspect an object handle returned by console_eval. Returns up to 50 key/value pairs.",
        schema(
            json!({
                "handle": { "type": "string", "description": "Handle from console_eval, e.g. \"@h1\"" },
                "ownProperties": prop("boolean", "Only own properties (default true)")
            }),
            &["handle"],
        ),
    ),
    |_ctx, args| console_get_props(&args)
);

fn console_get_props(args: &Args) -> Result<ToolResult, String> {
    args.required_str("handle")?;
    args.bool("ownProperties")?;
    // Runtime.getProperties on a remote object id.
    Err(unsupported("inspecting an object handle"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rever_core::traffic::{ConsoleEntry, RuntimeException};
    use rever_tools_api::tool::ToolContext;
    use serde_json::Value;

    use super::*;
    use crate::lock;
    use crate::test_support::{FakeBrowser, call, call_json, ctx_with};

    fn eval_ctx(result: Result<Value, String>) -> (Arc<FakeBrowser>, ToolContext) {
        let browser = Arc::new(FakeBrowser {
            evaluate: Some(result),
            ..FakeBrowser::default()
        });
        (browser.clone(), ctx_with(browser))
    }

    #[tokio::test]
    async fn 표현식을_페이지에서_평가해_값을_json으로_돌려준다() {
        let (browser, ctx) = eval_ctx(Ok(json!({ "title": "한글", "n": [1, 2] })));
        assert_eq!(
            call(&ConsoleEval, &ctx, json!({ "expression": "info()" })).await,
            Ok(r#"{"title":"한글","n":[1,2]}"#.to_owned())
        );
        assert_eq!(browser.calls(), ["evaluate:info()"]);

        // JSON.stringify of a string keeps its quotes.
        let (_, ctx) = eval_ctx(Ok(json!("a\"b")));
        assert_eq!(
            call(&ConsoleEval, &ctx, json!({ "expression": "s" })).await,
            Ok(r#""a\"b""#.to_owned())
        );
        let (_, ctx) = eval_ctx(Ok(json!(42)));
        assert_eq!(
            call(&ConsoleEval, &ctx, json!({ "expression": "6*7" })).await,
            Ok("42".to_owned())
        );
        // Numbers print as JavaScript prints them, whatever number type the
        // engine handed over.
        for (value, expected) in [
            (json!(2.0), "2"),
            (json!(1e21), "1e+21"),
            (json!(1e16), "10000000000000000"),
            (json!(0.000001), "0.000001"),
            (json!([1.0, 0.5]), "[1,0.5]"),
        ] {
            let (_, ctx) = eval_ctx(Ok(value));
            assert_eq!(
                call(&ConsoleEval, &ctx, json!({ "expression": "n" })).await,
                Ok(expected.to_owned())
            );
        }
    }

    #[tokio::test]
    async fn 예외와_지원하지_않는_엔진은_에러_결과다() {
        let (_, ctx) = eval_ctx(Err("ReferenceError: nope is not defined".to_owned()));
        assert_eq!(
            call(&ConsoleEval, &ctx, json!({ "expression": "nope" })).await,
            Err("ReferenceError: nope is not defined".to_owned())
        );
        let ctx = ToolContext::for_tests();
        assert_eq!(
            call(&ConsoleEval, &ctx, json!({ "expression": "1" })).await,
            Err(unsupported("evaluate"))
        );
    }

    #[tokio::test]
    async fn console_eval은_인자를_검증하고_context_id는_지원하지_않는다() {
        let (browser, ctx) = eval_ctx(Ok(json!(1)));
        assert!(call(&ConsoleEval, &ctx, json!({})).await.is_err());
        assert!(
            call(&ConsoleEval, &ctx, json!({ "expression": 1 }))
                .await
                .is_err()
        );
        assert!(
            call(
                &ConsoleEval,
                &ctx,
                json!({ "expression": "1", "contextId": 1.5 })
            )
            .await
            .is_err()
        );
        assert_eq!(
            call(
                &ConsoleEval,
                &ctx,
                json!({ "expression": "1", "contextId": 3 })
            )
            .await,
            Err(unsupported("evaluating in a chosen execution context"))
        );
        // Nothing was evaluated by a rejected call.
        assert!(browser.calls().is_empty());
    }

    fn log(ts: u64, text: &str) -> ConsoleEntry {
        ConsoleEntry {
            ts,
            kind: "log".to_owned(),
            text: text.to_owned(),
            ..ConsoleEntry::default()
        }
    }

    #[tokio::test]
    async fn 콘솔_로그를_오래된_순으로_돌려준다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            call(&ConsoleLogs, &ctx, json!({})).await,
            Ok("[]".to_owned())
        );
        {
            let mut store = lock(&ctx.traffic);
            store.append_console(log(100, "first"));
            store.append_console(ConsoleEntry {
                ts: 200,
                kind: "error".to_owned(),
                text: "boom".to_owned(),
                args: Some(vec![json!({ "type": "string", "value": "boom" })]),
                stack_trace: None,
            });
        }
        assert_eq!(
            call(&ConsoleLogs, &ctx, json!({})).await.unwrap(),
            r#"[
  {
    "ts": 100,
    "type": "log",
    "text": "first"
  },
  {
    "ts": 200,
    "type": "error",
    "text": "boom",
    "args": [
      {
        "type": "string",
        "value": "boom"
      }
    ]
  }
]"#
        );
    }

    #[tokio::test]
    async fn 콘솔_로그를_since와_limit로_고른다() {
        let ctx = ToolContext::for_tests();
        {
            let mut store = lock(&ctx.traffic);
            for n in 0..150_u64 {
                store.append_console(log(n, &n.to_string()));
            }
        }
        let texts = |logs: Value| -> Vec<String> {
            logs.as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry["text"].as_str().map(str::to_owned))
                .collect()
        };
        // Default: the newest 100.
        let default = texts(call_json(&ConsoleLogs, &ctx, json!({})).await);
        assert_eq!(default.len(), 100);
        assert_eq!(default.first().map(String::as_str), Some("50"));
        let last = texts(call_json(&ConsoleLogs, &ctx, json!({ "limit": 2 })).await);
        assert_eq!(last, ["148", "149"]);
        let since = texts(call_json(&ConsoleLogs, &ctx, json!({ "since": 147 })).await);
        assert_eq!(since, ["147", "148", "149"]);
        assert!(
            call(&ConsoleLogs, &ctx, json!({ "limit": 501 }))
                .await
                .is_err()
        );
        assert!(
            call(&ConsoleLogs, &ctx, json!({ "limit": 0 }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn 예외는_최근_것부터_limit개를_오래된_순으로_돌려준다() {
        let ctx = ToolContext::for_tests();
        {
            let mut store = lock(&ctx.traffic);
            for n in 0..60_u64 {
                store.append_exception(RuntimeException {
                    ts: n,
                    text: format!("e{n}"),
                    ..RuntimeException::default()
                });
            }
        }
        let default = call_json(&ConsoleExceptions, &ctx, json!({})).await;
        assert_eq!(default.as_array().map(Vec::len), Some(50));
        assert_eq!(default[0], json!({ "ts": 10, "text": "e10" }));
        let one = call_json(&ConsoleExceptions, &ctx, json!({ "limit": 1 })).await;
        assert_eq!(one, json!([{ "ts": 59, "text": "e59" }]));
        assert!(
            call(&ConsoleExceptions, &ctx, json!({ "limit": 201 }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn clear는_콘솔_로그만_비운다() {
        let ctx = ToolContext::for_tests();
        {
            let mut store = lock(&ctx.traffic);
            store.append_console(log(1, "x"));
            store.append_exception(RuntimeException::default());
        }
        assert_eq!(
            call(&ConsoleClear, &ctx, Value::Null).await,
            Ok("console logs cleared and handles reset".to_owned())
        );
        let store = lock(&ctx.traffic);
        assert_eq!(store.console_count(), 0);
        // clearConsole leaves the exceptions, as in the TypeScript.
        assert_eq!(store.exception_count(), 1);
    }

    #[tokio::test]
    async fn 핸들_조회는_인자를_검증한_뒤_지원하지_않는다고_알린다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            call(&ConsoleGetProps, &ctx, json!({ "handle": "@h1" })).await,
            Err(unsupported("inspecting an object handle"))
        );
        assert!(call(&ConsoleGetProps, &ctx, json!({})).await.is_err());
        assert_ne!(
            call(
                &ConsoleGetProps,
                &ctx,
                json!({ "handle": "@h1", "ownProperties": "no" })
            )
            .await,
            Err(unsupported("inspecting an object handle"))
        );
    }
}
