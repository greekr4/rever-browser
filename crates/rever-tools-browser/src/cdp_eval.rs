//! Port of `src/main/mcp/cdp-eval.ts` — evaluating in the page, and the overlay
//! calls that show the agent's actions on it.
//!
//! `Runtime.evaluate` over the debugger is `Browser::evaluate` here: the engine
//! awaits the value and turns a thrown exception into the `Err` text.

use std::sync::Arc;

use rever_tools_api::tool::Browser;
use serde_json::Value;

pub async fn eval_in_page(browser: &dyn Browser, expression: &str) -> Result<Value, String> {
    browser.evaluate(expression).await
}

pub fn js_literal(s: &str) -> String {
    Value::from(s).to_string()
}

/// Evaluate without waiting for it or letting it fail a tool.
///
/// The overlay is cosmetic. Awaiting it would queue behind whatever the page's
/// main thread is doing: typing three characters into a busy search box took 68
/// seconds that way, against 3 on an idle page. Without a tokio runtime to
/// spawn on, the call is dropped rather than awaited.
pub(crate) fn fire(browser: &Arc<dyn Browser>, expression: String) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        let browser = browser.clone();
        runtime.spawn(async move {
            let _ = browser.evaluate(&expression).await;
        });
    }
}

/// Fire a `window.__reverAi.<fn>(...)` overlay call into the page without
/// waiting for it or letting it fail a tool. Purely cosmetic, so a
/// not-yet-injected visualizer is silently ignored.
pub fn visualize(browser: &Arc<dyn Browser>, function: &str, args: &[Value]) {
    let arg_list: Vec<String> = args.iter().map(Value::to_string).collect();
    fire(
        browser,
        format!(
            "window.__reverAi && window.__reverAi.{function}({})",
            arg_list.join(",")
        ),
    );
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{FakeBrowser, context, settle_overlay};

    #[test]
    fn js_literal은_문자열을_자바스크립트_리터럴로_만든다() {
        assert_eq!(js_literal("a\"b\\c\n</x>한"), r#""a\"b\\c\n</x>한""#);
        assert_eq!(js_literal(""), "\"\"");
    }

    #[tokio::test]
    async fn 페이지에서_평가한_값과_오류를_그대로_돌려준다() {
        let (browser, ctx) = context(FakeBrowser::answering([
            Ok(json!({ "a": 1 })),
            Err("ReferenceError: x is not defined".to_owned()),
        ]));
        assert_eq!(
            eval_in_page(ctx.browser.as_ref(), "({a:1})").await,
            Ok(json!({ "a": 1 }))
        );
        assert_eq!(
            eval_in_page(ctx.browser.as_ref(), "x").await,
            Err("ReferenceError: x is not defined".to_owned())
        );
        assert_eq!(browser.calls(), ["evaluate:({a:1})", "evaluate:x"]);
    }

    #[tokio::test]
    async fn 오버레이_호출은_인자를_json으로_넣고_기다리지_않는다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!(1))]));
        visualize(&ctx.browser, "extractHighlight", &[json!("a.b"), json!(3)]);
        visualize(&ctx.browser, "shutter", &[]);
        visualize(&ctx.browser, "evalHudDone", &[json!("x"), json!(false)]);
        // Nothing has run yet: the calls were only spawned.
        assert!(browser.overlay().is_empty());
        settle_overlay().await;
        assert_eq!(
            browser.overlay(),
            [
                "window.__reverAi && window.__reverAi.extractHighlight(\"a.b\",3)",
                "window.__reverAi && window.__reverAi.shutter()",
                "window.__reverAi && window.__reverAi.evalHudDone(\"x\",false)",
            ]
        );
        // And they took none of the answers meant for the tool.
        assert_eq!(browser.evaluations.lock().unwrap().len(), 1);
    }

    #[test]
    fn 런타임이_없으면_오버레이_호출을_버린다() {
        let (browser, ctx) = context(FakeBrowser::new());
        visualize(&ctx.browser, "shutter", &[]);
        assert!(browser.overlay().is_empty());
    }
}
