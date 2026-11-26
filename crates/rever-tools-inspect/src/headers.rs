//! Port of `src/main/mcp/tools/headers.ts` — the `header_preset_*` tools.
//!
//! Applying a preset registers a request-intercept rule. The `Browser` trait
//! has no interception yet, so `header_preset_apply` and
//! `header_preset_disable` check their arguments and the preset as the
//! TypeScript did and then report the missing capability.

use std::sync::Mutex;

use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry, unsupported};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::args::validate;
use crate::js::{js_key_order, json_stringify};

#[derive(Debug, Clone, PartialEq)]
struct HeaderPreset {
    id: String,
    name: String,
    /// In the order the caller listed them.
    headers: Map<String, Value>,
    url_pattern: String,
    /// The intercept rule injecting this preset, while it is active.
    intercep_rule_id: Option<String>,
}

/// The saved presets, in the order they were first saved; the built-ins come
/// first. Fetched with `ctx.state::<HeaderPresets>()`.
pub struct HeaderPresets(Mutex<Vec<HeaderPreset>>);

impl Default for HeaderPresets {
    fn default() -> Self {
        let built_in = [
            (
                "localhost-trust",
                json!({ "X-Forwarded-For": "127.0.0.1", "X-Real-IP": "127.0.0.1", "X-Originating-IP": "127.0.0.1" }),
            ),
            (
                "mobile-safari-ua",
                json!({
                    "User-Agent": "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"
                }),
            ),
            (
                "googlebot",
                json!({
                    "User-Agent": "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"
                }),
            ),
            (
                "referer-trust",
                json!({ "Referer": "https://www.google.com/" }),
            ),
        ];
        let presets = built_in.into_iter().map(|(name, headers)| HeaderPreset {
            id: Uuid::new_v4().to_string(),
            name: name.to_owned(),
            headers: headers.as_object().cloned().unwrap_or_default(),
            url_pattern: "*".to_owned(),
            intercep_rule_id: None,
        });
        Self(Mutex::new(presets.collect()))
    }
}

impl HeaderPresets {
    fn with<R>(&self, f: impl FnOnce(&mut Vec<HeaderPreset>) -> R) -> R {
        f(&mut self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()))
    }
}

fn name_arg(args: &Value) -> &str {
    args.get("name").and_then(Value::as_str).unwrap_or("")
}

pub struct HeaderPresetList;

impl Tool for HeaderPresetList {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "header_preset_list",
            "List saved header presets (X-Forwarded-For, custom UA, Referer, Authorization). Built-ins ship with the app.",
            json!({
                "type": "object",
                "properties": {}
            }),
        )
    }

    // No argument check: the TypeScript registered this tool without an input
    // schema, so whatever was sent was accepted.
    fn call<'a>(&'a self, ctx: &'a ToolContext, _: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let list: Vec<Value> = ctx.state::<HeaderPresets>().with(|presets| {
                presets
                    .iter()
                    .map(|p| {
                        json!({
                            "id": p.id,
                            "name": p.name,
                            "urlPattern": p.url_pattern,
                            "headers": p.headers,
                            "active": p.intercep_rule_id.is_some(),
                        })
                    })
                    .collect()
            });
            ok(json_stringify(&Value::Array(list), true))
        })
    }
}

pub struct HeaderPresetSave;

impl Tool for HeaderPresetSave {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "header_preset_save",
            "Save a new header preset.",
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string"
                    },
                    "headers": {
                        "type": "object",
                        "propertyNames": {
                            "type": "string"
                        },
                        "additionalProperties": {
                            "type": "string"
                        }
                    },
                    "urlPattern": {
                        "description": "Default *",
                        "type": "string"
                    }
                },
                "required": ["name", "headers"],
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            let name = name_arg(&args);
            let preset = HeaderPreset {
                id: Uuid::new_v4().to_string(),
                name: name.to_owned(),
                // The TypeScript got the arguments from JSON.parse, which lists
                // integer-like names ("10", "2") first, in numeric order.
                headers: match args.get("headers").cloned().map(js_key_order) {
                    Some(Value::Object(headers)) => headers,
                    _ => Map::new(),
                },
                url_pattern: args
                    .get("urlPattern")
                    .and_then(Value::as_str)
                    .unwrap_or("*")
                    .to_owned(),
                intercep_rule_id: None,
            };
            let id = preset.id.clone();
            // Saving under an existing name replaces that preset in place.
            ctx.state::<HeaderPresets>().with(|presets| {
                match presets.iter_mut().find(|p| p.name == name) {
                    Some(existing) => *existing = preset,
                    None => presets.push(preset),
                }
            });
            ok(json_stringify(&json!({ "name": name, "id": id }), true))
        })
    }
}

pub struct HeaderPresetApply;

impl Tool for HeaderPresetApply {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "header_preset_apply",
            "[NOT SUPPORTED IN THIS BUILD] Activating a preset needs request interception, which the engine does not expose yet: a preset that is not already active answers that it is not supported. Activate a header preset — registers an intercept rule that injects the preset's headers into every matching outbound request.",
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string"
                    }
                },
                "required": ["name"],
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            let name = name_arg(&args);
            let active = ctx.state::<HeaderPresets>().with(|presets| {
                let preset = presets.iter().find(|p| p.name == name)?;
                Some(preset.intercep_rule_id.is_some())
            });
            match active {
                None => err(format!("unknown preset: {name}")),
                Some(true) => ok(format!("already active: {name}")),
                // The rule would go to the page's request interception.
                Some(false) => err(unsupported("request interception")),
            }
        })
    }
}

pub struct HeaderPresetDisable;

impl Tool for HeaderPresetDisable {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "header_preset_disable",
            "[NOT SUPPORTED IN THIS BUILD] Removing the rule needs request interception, which the engine does not expose yet. Deactivate a header preset (removes the intercept rule).",
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string"
                    }
                },
                "required": ["name"],
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            let name = name_arg(&args);
            let active = ctx.state::<HeaderPresets>().with(|presets| {
                presets
                    .iter()
                    .any(|p| p.name == name && p.intercep_rule_id.is_some())
            });
            if !active {
                return err(format!("not active: {name}"));
            }
            // Removing the rule needs the same interception the apply does.
            err(unsupported("request interception"))
        })
    }
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(HeaderPresetList);
    registry.register(HeaderPresetSave);
    registry.register(HeaderPresetApply);
    registry.register(HeaderPresetDisable);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;

    /// The preset list with every random id replaced by `<id>`, printed again.
    async fn list(ctx: &ToolContext) -> String {
        let (failed, text) = run(&HeaderPresetList, ctx, json!({})).await;
        assert!(!failed);
        let mut presets: Value = serde_json::from_str(&text).unwrap_or_default();
        for preset in presets.as_array_mut().into_iter().flatten() {
            let id = preset["id"].as_str().unwrap_or("");
            // A version 4 UUID, as randomUUID() makes.
            assert_eq!(id.len(), 36, "{id}");
            assert_eq!(id.chars().nth(14), Some('4'), "{id}");
            preset["id"] = json!("<id>");
        }
        let normalized = json_stringify(&presets, true);
        // Nothing but the ids changed: the tool printed this very shape.
        assert_eq!(
            normalized.len() - 4 * count(&presets),
            text.len() - 36 * count(&presets)
        );
        normalized
    }

    fn count(presets: &Value) -> usize {
        presets.as_array().map_or(0, Vec::len)
    }

    fn entry(name: &str, url_pattern: &str, headers: &str) -> String {
        format!(
            "  {{\n    \"id\": \"<id>\",\n    \"name\": \"{name}\",\n    \"urlPattern\": \"{url_pattern}\",\n    \"headers\": {headers},\n    \"active\": false\n  }}"
        )
    }

    fn built_ins() -> [String; 4] {
        [
            entry(
                "localhost-trust",
                "*",
                "{\n      \"X-Forwarded-For\": \"127.0.0.1\",\n      \"X-Real-IP\": \"127.0.0.1\",\n      \"X-Originating-IP\": \"127.0.0.1\"\n    }",
            ),
            entry(
                "mobile-safari-ua",
                "*",
                "{\n      \"User-Agent\": \"Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1\"\n    }",
            ),
            entry(
                "googlebot",
                "*",
                "{\n      \"User-Agent\": \"Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)\"\n    }",
            ),
            entry(
                "referer-trust",
                "*",
                "{\n      \"Referer\": \"https://www.google.com/\"\n    }",
            ),
        ]
    }

    fn listing(entries: &[String]) -> String {
        format!("[\n{}\n]", entries.join(",\n"))
    }

    async fn save(ctx: &ToolContext, args: Value) -> String {
        let (failed, text) = run(&HeaderPresetSave, ctx, args).await;
        assert!(!failed, "{text}");
        text
    }

    /// Marks a preset as applied, as a browser with request interception would.
    fn activate(ctx: &ToolContext, name: &str) {
        ctx.state::<HeaderPresets>().with(|presets| {
            for preset in presets.iter_mut().filter(|p| p.name == name) {
                preset.intercep_rule_id = Some("rule-1".into());
            }
        });
    }

    #[tokio::test]
    async fn 내장_프리셋_네_개를_순서대로_보여준다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(list(&ctx).await, listing(&built_ins()));
        // The tool has no input schema: any arguments are accepted.
        for args in [json!({ "anything": 1 }), Value::Null, json!("x")] {
            let (failed, text) = run(&HeaderPresetList, &ctx, args).await;
            assert!(!failed);
            assert!(text.starts_with("[\n  {\n    \"id\": \""));
        }
    }

    #[tokio::test]
    async fn 내장_프리셋의_id는_컨텍스트마다_새로_만들고_호출_사이에는_유지된다() {
        let ctx = ToolContext::for_tests();
        let first = run(&HeaderPresetList, &ctx, json!({})).await.1;
        assert_eq!(run(&HeaderPresetList, &ctx, json!({})).await.1, first);
        let other = ToolContext::for_tests();
        assert_ne!(run(&HeaderPresetList, &other, json!({})).await.1, first);
    }

    #[tokio::test]
    async fn 저장한_프리셋은_목록_끝에_붙고_헤더_순서는_보낸_그대로다() {
        let ctx = ToolContext::for_tests();
        let args = json!({ "name": "mine", "headers": { "X-B": "2", "X-A": "1" } });
        let saved = save(&ctx, args).await;
        let id = saved
            .strip_prefix("{\n  \"name\": \"mine\",\n  \"id\": \"")
            .and_then(|rest| rest.strip_suffix("\"\n}"));
        assert_eq!(id.map(str::len), Some(36), "{saved}");

        let mut expected = built_ins().to_vec();
        // urlPattern defaults to "*".
        expected.push(entry(
            "mine",
            "*",
            "{\n      \"X-B\": \"2\",\n      \"X-A\": \"1\"\n    }",
        ));
        assert_eq!(list(&ctx).await, listing(&expected));
        // The id in the answer is the stored one.
        let raw = run(&HeaderPresetList, &ctx, json!({})).await.1;
        assert!(raw.contains(id.unwrap_or("?")));
    }

    #[tokio::test]
    async fn 숫자_모양의_헤더_이름은_자바스크립트처럼_숫자_순서로_앞에_온다() {
        let ctx = ToolContext::for_tests();
        // What node listed after saving the same JSON text.
        let args = serde_json::from_str(
            r#"{ "name": "mine", "headers": { "b": "2", "10": "x", "2": "y", "a": "1", "01": "z" } }"#,
        )
        .unwrap_or_default();
        save(&ctx, args).await;
        let mut expected = built_ins().to_vec();
        expected.push(entry(
            "mine",
            "*",
            "{\n      \"2\": \"y\",\n      \"10\": \"x\",\n      \"b\": \"2\",\n      \"a\": \"1\",\n      \"01\": \"z\"\n    }",
        ));
        assert_eq!(list(&ctx).await, listing(&expected));
    }

    #[tokio::test]
    async fn 같은_이름으로_저장하면_제자리에서_바뀌고_id가_새로_생긴다() {
        let ctx = ToolContext::for_tests();
        let args = json!({ "name": "mine", "headers": { "X-B": "2" } });
        let first = save(&ctx, args).await;
        let args = json!({ "name": "googlebot", "headers": {}, "urlPattern": "https://a.test/*" });
        save(&ctx, args).await;
        let args = json!({ "name": "mine", "headers": { "X-C": "3" }, "urlPattern": "*api*" });
        let second = save(&ctx, args).await;
        assert_ne!(first, second);

        let mut expected = built_ins().to_vec();
        expected[2] = entry("googlebot", "https://a.test/*", "{}");
        expected.push(entry("mine", "*api*", "{\n      \"X-C\": \"3\"\n    }"));
        assert_eq!(list(&ctx).await, listing(&expected));
    }

    #[tokio::test]
    async fn 빈_이름과_빈_url_pattern도_그대로_저장한다() {
        let ctx = ToolContext::for_tests();
        let args = json!({ "name": "", "headers": { "k": "v" }, "urlPattern": "" });
        assert!(
            save(&ctx, args)
                .await
                .starts_with("{\n  \"name\": \"\",\n  \"id\": \"")
        );
        let mut expected = built_ins().to_vec();
        expected.push(entry("", "", "{\n      \"k\": \"v\"\n    }"));
        assert_eq!(list(&ctx).await, listing(&expected));
    }

    #[tokio::test]
    async fn 저장_인자가_틀리면_검증_오류다() {
        let ctx = ToolContext::for_tests();
        let (failed, text) = run(&HeaderPresetSave, &ctx, json!({})).await;
        assert!(failed);
        assert_eq!(
            text,
            "MCP error -32602: Input validation error: Invalid arguments for tool header_preset_save: [\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"name\"\n    ],\n    \"message\": \"Invalid input: expected string, received undefined\"\n  },\n  {\n    \"expected\": \"record\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"headers\"\n    ],\n    \"message\": \"Invalid input: expected record, received undefined\"\n  }\n]"
        );
        let args = json!({ "name": "x", "headers": { "a": 1, "b": null } });
        let (failed, text) = run(&HeaderPresetSave, &ctx, args).await;
        assert!(failed);
        assert!(text.contains("\"path\": [\n      \"headers\",\n      \"a\"\n    ],\n    \"message\": \"Invalid input: expected string, received number\""));
        assert!(text.contains("\"path\": [\n      \"headers\",\n      \"b\"\n    ],\n    \"message\": \"Invalid input: expected string, received null\""));
        let args = json!({ "name": "x", "headers": [] });
        let (_, text) = run(&HeaderPresetSave, &ctx, args).await;
        assert!(text.contains("Invalid input: expected record, received array"));
        // Nothing was saved by the rejected calls.
        assert_eq!(list(&ctx).await, listing(&built_ins()));
    }

    #[tokio::test]
    async fn 모르는_프리셋은_적용할_수_없다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            run(&HeaderPresetApply, &ctx, json!({ "name": "nope" })).await,
            (true, "unknown preset: nope".to_owned())
        );
        let (failed, text) = run(&HeaderPresetApply, &ctx, json!({})).await;
        assert!(failed);
        assert!(text.starts_with(
            "MCP error -32602: Input validation error: Invalid arguments for tool header_preset_apply: ["
        ));
    }

    #[tokio::test]
    async fn 적용은_요청_가로채기가_없어_지원하지_않는다고_알린다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            run(&HeaderPresetApply, &ctx, json!({ "name": "googlebot" })).await,
            (true, unsupported("request interception"))
        );
        // The preset is not marked active by the failed call.
        assert_eq!(list(&ctx).await, listing(&built_ins()));
    }

    #[tokio::test]
    async fn 이미_적용된_프리셋은_다시_적용하지_않는다() {
        let ctx = ToolContext::for_tests();
        activate(&ctx, "googlebot");
        assert_eq!(
            run(&HeaderPresetApply, &ctx, json!({ "name": "googlebot" })).await,
            (false, "already active: googlebot".to_owned())
        );
        let raw = run(&HeaderPresetList, &ctx, json!({})).await.1;
        assert_eq!(raw.matches("\"active\": true").count(), 1);
        assert_eq!(raw.matches("\"active\": false").count(), 3);
    }

    #[tokio::test]
    async fn 적용되지_않은_프리셋은_끌_수_없다() {
        let ctx = ToolContext::for_tests();
        for name in ["nope", "googlebot"] {
            assert_eq!(
                run(&HeaderPresetDisable, &ctx, json!({ "name": name })).await,
                (true, format!("not active: {name}"))
            );
        }
        let (failed, text) = run(&HeaderPresetDisable, &ctx, json!({})).await;
        assert!(failed);
        assert!(text.contains("Invalid input: expected string, received undefined"));

        activate(&ctx, "googlebot");
        assert_eq!(
            run(&HeaderPresetDisable, &ctx, json!({ "name": "googlebot" })).await,
            (true, unsupported("request interception"))
        );
    }

    #[tokio::test]
    async fn 적용된_프리셋을_다시_저장하면_새_프리셋은_꺼진_상태다() {
        let ctx = ToolContext::for_tests();
        activate(&ctx, "googlebot");
        save(&ctx, json!({ "name": "googlebot", "headers": {} })).await;
        assert_eq!(
            run(&HeaderPresetDisable, &ctx, json!({ "name": "googlebot" })).await,
            (true, "not active: googlebot".to_owned())
        );
    }
}
