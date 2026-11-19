//! Port of `src/main/mcp/tools/wait.ts` — `browser_wait_for`: poll the page until
//! a selector is visible and/or a text appears.

use rever_tools_api::tool::{ToolDef, ToolRegistry};
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::cdp_eval::{eval_in_page, js_literal};
use crate::{schema, sleep_ms, truthy};

pub fn register(registry: &mut ToolRegistry) {
    registry.register(BrowserWaitFor);
}

/// What was waited for: `{ selector, text }` without the ones not given, as
/// `JSON.stringify` drops undefined.
fn waited_for(selector: Option<&str>, text: Option<&str>) -> Value {
    let mut waited = serde_json::Map::new();
    if let Some(selector) = selector {
        waited.insert("selector".to_owned(), selector.into());
    }
    if let Some(text) = text {
        waited.insert("text".to_owned(), text.into());
    }
    Value::Object(waited)
}

tool!(
    BrowserWaitFor,
    ToolDef::new(
        "browser_wait_for",
        "Wait until the page settles before you read it — poll until a CSS selector is present (and visible) and/or a piece of text appears in the DOM. Use this on SPA / JS-rendered pages where the first snapshot shows a spinner or empty list because content arrives via a later XHR, instead of snapshotting repeatedly. Give at least one of selector / text; when both are given, both must hold.",
        schema(
            json!({
                "selector": {
                    "description": "CSS selector to wait for (must exist and be visible)",
                    "type": "string",
                },
                "text": {
                    "description": "Substring to wait for anywhere in the page text",
                    "type": "string",
                },
                "timeoutMs": {
                    "description": "Give up after this many ms (default 8000)",
                    "type": "integer",
                    "exclusiveMinimum": 0,
                    "maximum": 30000,
                },
            }),
            &[]
        )
    ),
    |ctx, args| {
        let (selector, text) = (args.opt_str("selector"), args.opt_str("text"));
        let timeout_ms = args.opt_number("timeoutMs").unwrap_or(8000.0);
        // An empty string asks for nothing, like the absent argument.
        let (wanted_selector, wanted_text) = (
            selector.filter(|selector| !selector.is_empty()),
            text.filter(|text| !text.is_empty()),
        );
        if wanted_selector.is_none() && wanted_text.is_none() {
            return Err("give at least one of `selector` or `text`".to_owned());
        }
        let mut checks: Vec<String> = Vec::new();
        if let Some(selector) = wanted_selector {
            // present AND visible (has layout boxes)
            checks.push(format!(
                "(() => {{ const el = document.querySelector({}); return !!el && el.getClientRects().length > 0; }})()",
                js_literal(selector)
            ));
        }
        if let Some(text) = wanted_text {
            checks.push(format!(
                "(document.body ? document.body.innerText : '').includes({})",
                js_literal(text)
            ));
        }
        let expr = checks.join(" && ");
        let start = Instant::now();
        let waited_ms = || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut polls = 0u64;
        while (waited_ms() as f64) < timeout_ms {
            polls += 1;
            let satisfied = eval_in_page(ctx.browser.as_ref(), &expr)
                .await
                .map_err(|e| format!("wait failed: {e}"))?;
            if truthy(&satisfied) {
                return Ok(json!({
                    "ready": true,
                    "waitedMs": waited_ms(),
                    "polls": polls,
                    "waitedFor": waited_for(selector, text),
                })
                .to_string());
            }
            sleep_ms(250.0).await;
        }
        Ok(json!({
            "ready": false,
            "timedOut": true,
            "waitedMs": waited_ms(),
            "waitedFor": waited_for(selector, text),
            "note": "Condition never became true within timeoutMs. The content may not load, the selector/text may be wrong, or it needs longer.",
        })
        .to_string())
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FakeBrowser, call, context};

    const SELECTOR_CHECK: &str = "(() => { const el = document.querySelector(\"#list li\"); return !!el && el.getClientRects().length > 0; })()";

    #[tokio::test(start_paused = true)]
    async fn 조건이_없으면_페이지에_묻지_않고_거절한다() {
        let (browser, ctx) = context(FakeBrowser::new());
        for args in [
            json!({}),
            json!({ "selector": "", "text": "" }),
            json!({ "timeoutMs": 100 }),
        ] {
            assert_eq!(
                call(&BrowserWaitFor, &ctx, args).await,
                Err("give at least one of `selector` or `text`".to_owned())
            );
        }
        assert!(browser.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn 선택자가_보이면_바로_준비됐다고_답한다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!(true))]));
        assert_eq!(
            call(&BrowserWaitFor, &ctx, json!({ "selector": "#list li" })).await,
            Ok("{\"ready\":true,\"waitedMs\":0,\"polls\":1,\"waitedFor\":{\"selector\":\"#list li\"}}".to_owned())
        );
        assert_eq!(browser.calls(), [format!("evaluate:{SELECTOR_CHECK}")]);
    }

    #[tokio::test(start_paused = true)]
    async fn 선택자와_글자를_함께_주면_둘_다_성립해야_한다() {
        let (browser, ctx) = context(FakeBrowser::answering([
            Ok(json!(false)),
            Ok(json!(false)),
            Ok(json!(true)),
        ]));
        let args = json!({ "selector": "#list li", "text": "완료 \"3\"" });
        assert_eq!(
            call(&BrowserWaitFor, &ctx, args).await,
            // Two failed polls, 250ms apart.
            Ok("{\"ready\":true,\"waitedMs\":500,\"polls\":3,\"waitedFor\":{\"selector\":\"#list li\",\"text\":\"완료 \\\"3\\\"\"}}".to_owned())
        );
        assert_eq!(
            browser.calls()[0],
            format!(
                "evaluate:{SELECTOR_CHECK} && (document.body ? document.body.innerText : '').includes(\"완료 \\\"3\\\"\")"
            )
        );
        assert_eq!(browser.calls().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn 시간_안에_성립하지_않으면_시간_초과를_알린다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let args = json!({ "text": "never", "timeoutMs": 600 });
        assert_eq!(
            call(&BrowserWaitFor, &ctx, args).await,
            Ok("{\"ready\":false,\"timedOut\":true,\"waitedMs\":750,\"waitedFor\":{\"text\":\"never\"},\"note\":\"Condition never became true within timeoutMs. The content may not load, the selector/text may be wrong, or it needs longer.\"}".to_owned())
        );
        // Polled at 0, 250 and 500ms.
        assert_eq!(browser.calls().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn 기본_제한_시간은_8초다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let text = call(&BrowserWaitFor, &ctx, json!({ "text": "never" }))
            .await
            .unwrap();
        assert!(text.starts_with("{\"ready\":false,\"timedOut\":true,\"waitedMs\":8000,"));
        assert_eq!(browser.calls().len(), 32);
    }

    #[tokio::test(start_paused = true)]
    async fn 페이지_오류는_기다림을_멈추고_오류로_알린다() {
        let (_, ctx) = context(FakeBrowser::answering([
            Ok(json!(false)),
            Err("SyntaxError: '[' is not a valid selector".to_owned()),
        ]));
        assert_eq!(
            call(&BrowserWaitFor, &ctx, json!({ "selector": "[" })).await,
            Err("wait failed: SyntaxError: '[' is not a valid selector".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 참으로_볼_수_있는_값이면_성립한_것이다() {
        let (_, ctx) = context(FakeBrowser::answering([Ok(json!(0)), Ok(json!("yes"))]));
        let text = call(&BrowserWaitFor, &ctx, json!({ "text": "x" }))
            .await
            .unwrap();
        assert!(text.starts_with("{\"ready\":true,\"waitedMs\":250,\"polls\":2,"));
    }
}
