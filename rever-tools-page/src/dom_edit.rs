//! Port of `src/main/mcp/tools/dom_edit.ts` — attribute, style, text and removal
//! edits on the elements a CSS selector matches.

use rever_tools_api::tool::{ToolContext, ToolDef, ToolRegistry};
use serde_json::{Value, json};

use crate::{js_str, schema};

pub fn register(registry: &mut ToolRegistry) {
    registry.register(DomSetAttr);
    registry.register(DomSetStyle);
    registry.register(DomSetText);
    registry.register(DomRemove);
}

/// The expression's value as the TypeScript interpolated it: a string as is, nothing
/// for `undefined`, anything else in its JSON form.
async fn run_expr(ctx: &ToolContext, expr: &str) -> Result<String, String> {
    Ok(match ctx.browser.evaluate(expr).await? {
        Value::String(text) => text,
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

fn string() -> Value {
    json!({ "type": "string" })
}

tool!(
    DomSetAttr,
    ToolDef::new(
        "dom_set_attr",
        "Set an attribute on the first element matching a CSS selector. Useful for CSS-puzzle challenges (e.g. moving an element via style.left).",
        schema(
            json!({ "selector": string(), "attr": string(), "value": string() }),
            &["selector", "attr", "value"]
        )
    ),
    |ctx, args| {
        let (selector, attr, value) = (args.str("selector"), args.str("attr"), args.str("value"));
        let expr = format!(
            "(() => {{
  const el = document.querySelector({});
  if (!el) return 'not-found';
  el.setAttribute({}, {});
  return 'ok';
}})()",
            js_str(selector),
            js_str(attr),
            js_str(value)
        );
        if run_expr(ctx, &expr).await? == "not-found" {
            return Err(format!("no element matches selector: {selector}"));
        }
        Ok(format!("{selector}[{attr}]={value}"))
    }
);

tool!(
    DomSetStyle,
    ToolDef::new(
        "dom_set_style",
        "Set an inline CSS style property on the first match.",
        schema(
            json!({ "selector": string(), "property": string(), "value": string() }),
            &["selector", "property", "value"]
        )
    ),
    |ctx, args| {
        let (selector, property, value) = (
            args.str("selector"),
            args.str("property"),
            args.str("value"),
        );
        let expr = format!(
            "(() => {{
  const el = document.querySelector({});
  if (!el) return 'not-found';
  el.style.setProperty({}, {});
  return 'ok';
}})()",
            js_str(selector),
            js_str(property),
            js_str(value)
        );
        if run_expr(ctx, &expr).await? == "not-found" {
            return Err(format!("no element matches selector: {selector}"));
        }
        Ok(format!("{selector}.style.{property}={value}"))
    }
);

tool!(
    DomSetText,
    ToolDef::new(
        "dom_set_text",
        "Set textContent on the first match.",
        schema(
            json!({ "selector": string(), "text": string() }),
            &["selector", "text"]
        )
    ),
    |ctx, args| {
        let (selector, text) = (args.str("selector"), args.str("text"));
        let expr = format!(
            "(() => {{
  const el = document.querySelector({});
  if (!el) return 'not-found';
  el.textContent = {};
  return 'ok';
}})()",
            js_str(selector),
            js_str(text)
        );
        if run_expr(ctx, &expr).await? == "not-found" {
            return Err(format!("no element matches: {selector}"));
        }
        Ok("text set".to_owned())
    }
);

tool!(
    DomRemove,
    ToolDef::new(
        "dom_remove",
        "Remove all elements matching a CSS selector from the DOM.",
        schema(json!({ "selector": string() }), &["selector"])
    ),
    |ctx, args| {
        let expr = format!(
            "(() => {{
  const els = document.querySelectorAll({});
  els.forEach((e) => e.remove());
  return els.length;
}})()",
            js_str(args.str("selector"))
        );
        Ok(format!(
            "removed {} element(s)",
            run_expr(ctx, &expr).await?
        ))
    }
);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{FakeBrowser, call, context, invalid, rejected};

    fn found() -> FakeBrowser {
        FakeBrowser::answering([Ok(json!("ok"))])
    }

    fn not_found() -> FakeBrowser {
        FakeBrowser::answering([Ok(json!("not-found"))])
    }

    #[tokio::test]
    async fn 속성을_설정하고_인자는_문자열_리터럴로_넣는다() {
        let (browser, ctx) = context(found());
        let args =
            json!({ "selector": "a[href=\"x\"]", "attr": "data-k", "value": "1\n';alert(1)//" });
        assert_eq!(
            call(&DomSetAttr, &ctx, args).await,
            Ok("a[href=\"x\"][data-k]=1\n';alert(1)//".to_owned())
        );
        assert_eq!(
            browser.calls(),
            [concat!(
                "evaluate:(() => {\n",
                "  const el = document.querySelector(\"a[href=\\\"x\\\"]\");\n",
                "  if (!el) return 'not-found';\n",
                "  el.setAttribute(\"data-k\", \"1\\n';alert(1)//\");\n",
                "  return 'ok';\n",
                "})()"
            )]
        );
    }

    #[tokio::test]
    async fn 맞는_요소가_없으면_오류로_알린다() {
        let (_, ctx) = context(not_found());
        let args = json!({ "selector": "#x", "attr": "a", "value": "v" });
        assert_eq!(
            call(&DomSetAttr, &ctx, args).await,
            Err("no element matches selector: #x".to_owned())
        );

        let (_, ctx) = context(not_found());
        let args = json!({ "selector": "#x", "property": "left", "value": "1px" });
        assert_eq!(
            call(&DomSetStyle, &ctx, args).await,
            Err("no element matches selector: #x".to_owned())
        );

        // dom_set_text words it differently in the TypeScript.
        let (_, ctx) = context(not_found());
        assert_eq!(
            call(&DomSetText, &ctx, json!({ "selector": "#x", "text": "t" })).await,
            Err("no element matches: #x".to_owned())
        );
    }

    #[tokio::test]
    async fn 인라인_스타일을_설정한다() {
        let (browser, ctx) = context(found());
        let args = json!({ "selector": ".box", "property": "left", "value": "10px" });
        assert_eq!(
            call(&DomSetStyle, &ctx, args).await,
            Ok(".box.style.left=10px".to_owned())
        );
        assert!(browser.calls()[0].contains("el.style.setProperty(\"left\", \"10px\");"));
    }

    #[tokio::test]
    async fn 텍스트를_설정한다() {
        let (browser, ctx) = context(found());
        assert_eq!(
            call(
                &DomSetText,
                &ctx,
                json!({ "selector": "h1", "text": "한\"글" })
            )
            .await,
            Ok("text set".to_owned())
        );
        assert!(browser.calls()[0].contains("el.textContent = \"한\\\"글\";"));
    }

    #[tokio::test]
    async fn 맞는_요소를_모두_지우고_개수를_알린다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!(3))]));
        assert_eq!(
            call(&DomRemove, &ctx, json!({ "selector": ".ad" })).await,
            Ok("removed 3 element(s)".to_owned())
        );
        assert!(browser.calls()[0].contains("document.querySelectorAll(\".ad\");"));

        // `r.result.value ?? ''` in the TypeScript.
        let (_, ctx) = context(FakeBrowser::default());
        assert_eq!(
            call(&DomRemove, &ctx, json!({ "selector": ".ad" })).await,
            Ok("removed  element(s)".to_owned())
        );
    }

    #[tokio::test]
    async fn 페이지에서_난_오류는_오류_결과가_된다() {
        let (_, ctx) = context(FakeBrowser::answering([Err(
            "SyntaxError: bad selector".to_owned()
        )]));
        assert_eq!(
            call(&DomRemove, &ctx, json!({ "selector": "[" })).await,
            Err("SyntaxError: bad selector".to_owned())
        );
    }

    #[tokio::test]
    async fn 빠지거나_문자열이_아닌_인자는_페이지에_닿기_전에_거절한다() {
        let (browser, ctx) = context(FakeBrowser::default());
        assert_eq!(
            call(&DomSetAttr, &ctx, json!({ "selector": "a", "attr": "b" })).await,
            invalid("dom_set_attr", "value", "string", "undefined")
        );
        assert_eq!(
            call(&DomSetStyle, &ctx, json!({ "selector": 1 })).await,
            // Every problem is listed, in the order of the schema.
            rejected(
                "dom_set_style",
                json!([
                    {
                        "expected": "string", "code": "invalid_type", "path": ["selector"],
                        "message": "Invalid input: expected string, received number"
                    },
                    {
                        "expected": "string", "code": "invalid_type", "path": ["property"],
                        "message": "Invalid input: expected string, received undefined"
                    },
                    {
                        "expected": "string", "code": "invalid_type", "path": ["value"],
                        "message": "Invalid input: expected string, received undefined"
                    }
                ])
            )
        );
        assert_eq!(
            call(&DomSetText, &ctx, json!({ "selector": "a", "text": null })).await,
            invalid("dom_set_text", "text", "string", "null")
        );
        assert_eq!(
            call(&DomRemove, &ctx, json!(null)).await,
            // No arguments at all.
            rejected(
                "dom_remove",
                json!([{
                    "expected": "object", "code": "invalid_type", "path": [],
                    "message": "Invalid input: expected object, received undefined"
                }])
            )
        );
        assert!(browser.calls().is_empty());
    }
}
