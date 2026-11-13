//! Port of `src/main/mcp/tools/dialog.ts` — the page's alert/confirm/prompt history
//! and the in-page override that records it.
//!
//! The TypeScript also kept a history of native dialogs and an auto-dismiss switch,
//! both fed by Chrome DevTools Protocol events. `Browser` has no native dialog
//! events, so those parts report the operation as unsupported.

use rever_tools_api::tool::{ToolDef, ToolRegistry, unsupported};
use serde_json::{Value, json};

use crate::{no_args, pretty, schema};

const NATIVE_DIALOGS: &str = "native dialog handling";

const INJECT_OVERRIDE: &str = r#"(() => {
  window.__revDialogOverride = undefined;
  window.__revDialogHistory = window.__revDialogHistory || [];
  function record(type, message, def) {
    const entry = { type, message: String(message ?? ''), ts: Date.now() };
    if (def !== undefined) entry.default = String(def);
    window.__revDialogHistory.push(entry);
    try { console.log('[rev-' + type + ']', message); } catch(e) {}
  }
  window.alert = function(msg) { record('alert', msg); };
  window.confirm = function(msg) { record('confirm', msg); return true; };
  window.prompt = function(msg, def) { record('prompt', msg, def); return def == null ? '' : String(def); };
  return 'ok';
})()"#;

pub fn register(registry: &mut ToolRegistry) {
    registry.register(DialogHistory);
    registry.register(DialogSetAutoDismiss);
    registry.register(DialogClearHistory);
    registry.register(DialogInjectOverride);
}

tool!(
    DialogHistory,
    ToolDef::new(
        "dialog_history",
        "List recent JavaScript dialog events (alert/confirm/prompt) that the page tried to open. Useful to see what message a challenge popped — the agent never sees the modal because rever-browser auto-dismisses, but the content is recorded here. Also queries the in-page history captured by the script override.",
        schema(
            json!({
                "limit": {
                    "description": "Default 50",
                    "type": "integer",
                    "exclusiveMinimum": 0,
                    "maximum": 200
                }
            }),
            &[]
        )
    ),
    |ctx, _args| {
        // `limit` only ever applied to the native history, which is empty here.
        // Page-side history (from the injected override). A page that cannot answer
        // has none: this never fails the call.
        let page_history = match ctx
            .browser
            .evaluate("JSON.stringify(window.__revDialogHistory || [])")
            .await
        {
            Ok(Value::String(history)) => serde_json::from_str(&history).ok(),
            _ => None,
        };
        Ok(pretty(&json!({
            // The TypeScript default; nothing can change it without native dialog handling.
            "autoDismiss": true,
            "cdpHistory": [],
            "pageHistory": page_history.unwrap_or_else(|| json!([])),
        })))
    }
);

tool!(
    DialogSetAutoDismiss,
    ToolDef::new(
        "dialog_set_auto_dismiss",
        "Toggle whether native JS dialogs are auto-dismissed. Default true (recommended). Turn off if you want to see real alert/confirm/prompt modals.",
        schema(json!({ "enabled": { "type": "boolean" } }), &["enabled"])
    ),
    |_ctx, _args| { Err(unsupported(NATIVE_DIALOGS)) }
);

tool!(
    DialogClearHistory,
    ToolDef::new(
        "dialog_clear_history",
        "Clear the CDP-side dialog history buffer.",
        no_args()
    ),
    |_ctx, _args| { Err(unsupported(NATIVE_DIALOGS)) }
);

tool!(
    DialogInjectOverride,
    ToolDef::new(
        "dialog_inject_override",
        "Re-inject the alert/confirm/prompt override into the active page. Useful if a page replaced window.alert with its own implementation after load.",
        no_args()
    ),
    |ctx, _args| {
        ctx.browser.evaluate(INJECT_OVERRIDE).await?;
        Ok("re-injected dialog override on current page".to_owned())
    }
);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{
        FakeBrowser, call, context, invalid, not_int, rejected, too_big, too_small,
    };

    #[tokio::test]
    async fn 페이지가_기록한_다이얼로그_내역을_돌려준다() {
        let history = r#"[{"type":"alert","message":"hi","ts":1}]"#;
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!(history))]));
        assert_eq!(
            call(&DialogHistory, &ctx, json!({ "limit": 200 })).await,
            Ok(concat!(
                "{\n",
                "  \"autoDismiss\": true,\n",
                "  \"cdpHistory\": [],\n",
                "  \"pageHistory\": [\n",
                "    {\n",
                "      \"type\": \"alert\",\n",
                "      \"message\": \"hi\",\n",
                "      \"ts\": 1\n",
                "    }\n",
                "  ]\n",
                "}"
            )
            .to_owned())
        );
        assert_eq!(
            browser.calls(),
            ["evaluate:JSON.stringify(window.__revDialogHistory || [])"]
        );
    }

    #[tokio::test]
    async fn 페이지_내역을_읽지_못하면_빈_목록으로_답한다() {
        let empty = "{\n  \"autoDismiss\": true,\n  \"cdpHistory\": [],\n  \"pageHistory\": []\n}";
        for answer in [
            Err("no page".to_owned()),
            Ok(Value::Null),
            Ok(json!("not json")),
        ] {
            let (_, ctx) = context(FakeBrowser::answering([answer]));
            assert_eq!(
                call(&DialogHistory, &ctx, json!({})).await,
                Ok(empty.to_owned())
            );
        }
    }

    #[tokio::test]
    async fn limit은_1에서_200_사이의_정수만_받는다() {
        let (browser, ctx) = context(FakeBrowser::default());
        for (limit, issue) in [
            (json!(0), too_small("limit")),
            (json!(-3), too_small("limit")),
            (json!(201), too_big("limit", 200)),
            (json!(1.5), not_int("limit")),
            // A fraction is reported alone, even when it is out of range too.
            (json!(-1.5), not_int("limit")),
        ] {
            assert_eq!(
                call(&DialogHistory, &ctx, json!({ "limit": limit })).await,
                rejected("dialog_history", json!([issue]))
            );
        }
        for (limit, received) in [(json!("5"), "string"), (json!(null), "null")] {
            assert_eq!(
                call(&DialogHistory, &ctx, json!({ "limit": limit })).await,
                invalid("dialog_history", "limit", "number", received)
            );
        }
        // A rejected call never reaches the page.
        assert!(browser.calls().is_empty());
        for limit in [json!(1), json!(200), json!(200.0)] {
            assert!(
                call(&DialogHistory, &ctx, json!({ "limit": limit }))
                    .await
                    .is_ok()
            );
        }
    }

    #[tokio::test]
    async fn 안전한_정수_범위를_벗어난_limit은_두_가지_문제를_함께_알린다() {
        let (_, ctx) = context(FakeBrowser::default());
        let note = "Integers must be within the safe integer range.";
        assert_eq!(
            call(&DialogHistory, &ctx, json!({ "limit": 1e20 })).await,
            rejected(
                "dialog_history",
                json!([
                    {
                        "code": "too_big", "maximum": 9007199254740991_i64, "note": note,
                        "origin": "int", "inclusive": true, "path": ["limit"],
                        "message": "Too big: expected int to be <=9007199254740991"
                    },
                    too_big("limit", 200)
                ])
            )
        );
        assert_eq!(
            call(&DialogHistory, &ctx, json!({ "limit": -1e20 })).await,
            rejected(
                "dialog_history",
                json!([
                    {
                        "code": "too_small", "minimum": -9007199254740991_i64, "note": note,
                        "origin": "int", "inclusive": true, "path": ["limit"],
                        "message": "Too small: expected int to be >=-9007199254740991"
                    },
                    too_small("limit")
                ])
            )
        );
    }

    #[tokio::test]
    async fn 자동_닫기_전환은_인자를_검사한_뒤_지원하지_않는다고_알린다() {
        let (_, ctx) = context(FakeBrowser::default());
        for (args, received) in [
            (json!({}), "undefined"),
            (json!({ "enabled": "yes" }), "string"),
            (json!({ "enabled": null }), "null"),
        ] {
            assert_eq!(
                call(&DialogSetAutoDismiss, &ctx, args).await,
                invalid("dialog_set_auto_dismiss", "enabled", "boolean", received)
            );
        }
        assert_eq!(
            call(&DialogSetAutoDismiss, &ctx, json!({ "enabled": false })).await,
            Err(unsupported("native dialog handling"))
        );
    }

    #[tokio::test]
    async fn 네이티브_내역_지우기는_지원하지_않는다고_알린다() {
        let (_, ctx) = context(FakeBrowser::default());
        assert_eq!(
            call(&DialogClearHistory, &ctx, json!({})).await,
            Err(unsupported("native dialog handling"))
        );
    }

    #[tokio::test]
    async fn 오버라이드를_현재_페이지에_다시_주입한다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!("ok"))]));
        assert_eq!(
            call(&DialogInjectOverride, &ctx, json!({})).await,
            Ok("re-injected dialog override on current page".to_owned())
        );
        let calls = browser.calls();
        assert_eq!(calls.len(), 1);
        for part in [
            "window.__revDialogOverride = undefined;",
            "window.alert = function(msg) { record('alert', msg); };",
            "window.confirm = function(msg) { record('confirm', msg); return true; };",
            "return def == null ? '' : String(def);",
        ] {
            assert!(calls[0].contains(part), "{part}");
        }
    }

    #[tokio::test]
    async fn 주입이_실패하면_엔진의_오류를_그대로_돌려준다() {
        let (_, ctx) = context(FakeBrowser::answering([
            Err("the tab is closed".to_owned()),
        ]));
        assert_eq!(
            call(&DialogInjectOverride, &ctx, json!({})).await,
            Err("the tab is closed".to_owned())
        );
    }
}
