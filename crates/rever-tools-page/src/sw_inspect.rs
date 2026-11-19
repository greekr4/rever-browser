//! Port of `src/main/mcp/tools/sw_inspect.ts` — Service Worker registrations and
//! Cache API entries of the active page.
//!
//! Every expression here is an async function call: the TypeScript evaluated them
//! with `awaitPromise`, so `Browser::evaluate` has to resolve the promise.

use rever_tools_api::tool::{ToolDef, ToolRegistry};
use serde_json::{Value, json};

use crate::{js_str, no_args, pretty, schema};

pub fn register(registry: &mut ToolRegistry) {
    registry.register(SwList);
    registry.register(SwUnregister);
    registry.register(SwCaches);
}

const LIST: &str = "(async () => {
  if (!navigator.serviceWorker) return { error: 'not supported' };
  const regs = await navigator.serviceWorker.getRegistrations();
  return regs.map(r => ({
    scope: r.scope,
    active: r.active ? { scriptURL: r.active.scriptURL, state: r.active.state } : null,
    waiting: r.waiting ? { scriptURL: r.waiting.scriptURL, state: r.waiting.state } : null,
    installing: r.installing ? { scriptURL: r.installing.scriptURL, state: r.installing.state } : null
  }));
})()";

const CACHES: &str = "(async () => {
  if (!('caches' in self)) return { error: 'caches API not available' };
  const names = await caches.keys();
  const out = {};
  for (const n of names) {
    const c = await caches.open(n);
    const reqs = await c.keys();
    out[n] = reqs.map(r => r.url);
  }
  return out;
})()";

tool!(
    SwList,
    ToolDef::new(
        "sw_list",
        "List Service Worker registrations on the active page (scope, active script URL, state).",
        no_args()
    ),
    |ctx, _args| { Ok(pretty(&ctx.browser.evaluate(LIST).await?)) }
);

tool!(
    SwUnregister,
    ToolDef::new(
        "sw_unregister",
        "Unregister a Service Worker by scope. Useful when SW caching interferes with reversing.",
        schema(
            json!({ "scope": { "type": "string", "description": "Scope URL" } }),
            &["scope"]
        )
    ),
    |ctx, args| {
        let scope = args.str("scope");
        let expr = format!(
            "(async () => {{
  const reg = await navigator.serviceWorker.getRegistration({});
  if (!reg) return 'not-found';
  return await reg.unregister() ? 'ok' : 'failed';
}})()",
            js_str(scope)
        );
        let outcome = match ctx.browser.evaluate(&expr).await? {
            Value::String(text) => text,
            other => other.to_string(),
        };
        Ok(format!("unregister({scope}) → {outcome}"))
    }
);

tool!(
    SwCaches,
    ToolDef::new(
        "sw_caches",
        "List Cache API entries (Service Worker / fetch caches) for the active page.",
        no_args()
    ),
    |ctx, _args| { Ok(pretty(&ctx.browser.evaluate(CACHES).await?)) }
);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{FakeBrowser, call, context, invalid};

    #[tokio::test]
    async fn 서비스_워커_등록을_들여쓴_json으로_돌려준다() {
        let registrations = json!([{
            "scope": "https://a.test/",
            "active": { "scriptURL": "https://a.test/sw.js", "state": "activated" },
            "waiting": null,
            "installing": null
        }]);
        let (browser, ctx) = context(FakeBrowser::answering([Ok(registrations)]));
        assert_eq!(
            call(&SwList, &ctx, json!({})).await,
            Ok(concat!(
                "[\n",
                "  {\n",
                "    \"scope\": \"https://a.test/\",\n",
                "    \"active\": {\n",
                "      \"scriptURL\": \"https://a.test/sw.js\",\n",
                "      \"state\": \"activated\"\n",
                "    },\n",
                "    \"waiting\": null,\n",
                "    \"installing\": null\n",
                "  }\n",
                "]"
            )
            .to_owned())
        );
        let calls = browser.calls();
        assert!(calls[0].starts_with("evaluate:(async () => {"));
        assert!(calls[0].contains("await navigator.serviceWorker.getRegistrations();"));
    }

    #[tokio::test]
    async fn 지원하지_않는_페이지의_답도_그대로_돌려준다() {
        let (_, ctx) = context(FakeBrowser::answering([
            Ok(json!({ "error": "not supported" })),
            Ok(json!({ "error": "caches API not available" })),
        ]));
        assert_eq!(
            call(&SwList, &ctx, json!({})).await,
            Ok("{\n  \"error\": \"not supported\"\n}".to_owned())
        );
        assert_eq!(
            call(&SwCaches, &ctx, json!({})).await,
            Ok("{\n  \"error\": \"caches API not available\"\n}".to_owned())
        );
    }

    #[tokio::test]
    async fn 범위로_서비스_워커를_해제한다() {
        let (browser, ctx) = context(FakeBrowser::answering([
            Ok(json!("ok")),
            Ok(json!("not-found")),
        ]));
        assert_eq!(
            call(
                &SwUnregister,
                &ctx,
                json!({ "scope": "https://a.test/\"x" })
            )
            .await,
            Ok("unregister(https://a.test/\"x) → ok".to_owned())
        );
        // Not finding the registration is an answer, not an error, in the TypeScript.
        assert_eq!(
            call(&SwUnregister, &ctx, json!({ "scope": "https://b.test/" })).await,
            Ok("unregister(https://b.test/) → not-found".to_owned())
        );
        assert!(
            browser.calls()[0].contains(
                "await navigator.serviceWorker.getRegistration(\"https://a.test/\\\"x\");"
            )
        );
    }

    #[tokio::test]
    async fn 범위가_없으면_거절한다() {
        let (browser, ctx) = context(FakeBrowser::default());
        assert_eq!(
            call(&SwUnregister, &ctx, json!({})).await,
            invalid("sw_unregister", "scope", "string", "undefined")
        );
        assert!(browser.calls().is_empty());
    }

    #[tokio::test]
    async fn 캐시_항목을_캐시_이름별로_돌려준다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(
            json!({ "v1": ["https://a.test/app.js"], "empty": [] }),
        )]));
        assert_eq!(
            call(&SwCaches, &ctx, json!({})).await,
            Ok(concat!(
                "{\n",
                "  \"v1\": [\n",
                "    \"https://a.test/app.js\"\n",
                "  ],\n",
                "  \"empty\": []\n",
                "}"
            )
            .to_owned())
        );
        assert!(browser.calls()[0].contains("const names = await caches.keys();"));
    }

    #[tokio::test]
    async fn 페이지에서_난_오류는_오류_결과가_된다() {
        for tool in [&SwList as &dyn rever_tools_api::tool::Tool, &SwCaches] {
            let (_, ctx) = context(FakeBrowser::answering([Err("SecurityError".to_owned())]));
            assert_eq!(
                call(tool, &ctx, json!({})).await,
                Err("SecurityError".to_owned())
            );
        }
        let (_, ctx) = context(FakeBrowser::answering([Err("TypeError: x".to_owned())]));
        assert_eq!(
            call(&SwUnregister, &ctx, json!({ "scope": "s" })).await,
            Err("TypeError: x".to_owned())
        );
    }
}
