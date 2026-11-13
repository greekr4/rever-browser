//! MCP tools acting on the page: dialogs, DOM edits, storage, service workers,
//! injection, vision, overrides, downloads.

use rever_tools_api::tool::{ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Value, json};

/// Declares a unit-struct tool: its definition, and a body that yields the result
/// text as `Result<String, String>` (`Err` becomes an `isError` result). The body is
/// an async block, so `?` works in it; it only runs for arguments that passed
/// [`validate`], and `$args` is an [`Args`] over them.
macro_rules! tool {
    ($name:ident, $def:expr, |$ctx:ident, $args:ident| $body:block) => {
        pub struct $name;

        impl rever_tools_api::tool::Tool for $name {
            fn def(&self) -> rever_tools_api::tool::ToolDef {
                $def
            }

            fn call<'a>(
                &'a self,
                $ctx: &'a rever_tools_api::tool::ToolContext,
                args: serde_json::Value,
            ) -> rever_tools_api::tool::BoxFuture<'a, rever_tools_api::utils::ToolResult> {
                Box::pin(async move {
                    if let Err(text) = $crate::validate(&self.def(), &args) {
                        return rever_tools_api::utils::err(text);
                    }
                    let $args = &$crate::Args(args);
                    let result: Result<String, String> = async { $body }.await;
                    $crate::done(result)
                })
            }
        }
    };
}

pub mod dialog;
pub mod dom_edit;
pub mod downloads;
pub mod inject;
pub mod r#override;
pub mod storage;
pub mod sw_inspect;
pub mod vision;

/// Registers every tool of this crate, in the order `buildMcpServer` does.
pub fn register(registry: &mut ToolRegistry) {
    inject::register(registry);
    storage::register(registry);
    r#override::register(registry);
    dom_edit::register(registry);
    sw_inspect::register(registry);
    dialog::register(registry);
    downloads::register(registry);
    vision::register(registry);
}

pub(crate) fn done(result: Result<String, String>) -> ToolResult {
    match result {
        Ok(text) => ok(text),
        Err(text) => err(text),
    }
}

/// The schema the MCP SDK lists for a tool registered without `inputSchema`.
pub(crate) fn no_args() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// The schema the MCP SDK derives from a zod shape.
pub(crate) fn schema(properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": properties,
    });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

/// `JSON.stringify(value, null, 2)`.
pub(crate) fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// `JSON.stringify(text)`: a string as a JavaScript string literal.
pub(crate) fn js_str(text: &str) -> String {
    Value::from(text).to_string()
}

// Number.MAX_SAFE_INTEGER: zod's `.int()` only accepts integers a double holds exactly.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Checks the arguments against the tool's own schema the way the MCP SDK does
/// before a handler runs: every problem at once, in the order of the schema, in the
/// words zod uses, because that text is what the agent reads and corrects from.
/// `Err` carries the text of the `isError` result. It knows the constructs these
/// tools use: string, enum, boolean, number, bounded integer and array of strings.
pub(crate) fn validate(def: &ToolDef, args: &Value) -> Result<(), String> {
    let schema = &def.input_schema;
    // The SDK does not look at the arguments of a tool registered without a schema.
    let Some(properties) = schema
        .get("properties")
        .and_then(Value::as_object)
        .filter(|properties| !properties.is_empty())
    else {
        return Ok(());
    };
    let mut issues = Vec::new();
    match args {
        Value::Object(fields) => {
            let required = schema.get("required").and_then(Value::as_array);
            for (key, property) in properties {
                let field = fields.get(key);
                let is_required = required
                    .is_some_and(|names| names.iter().any(|name| name.as_str() == Some(key)));
                // Optional means absent; null is a value of the wrong type.
                if field.is_some() || is_required {
                    check(property, field, &mut vec![json!(key)], &mut issues);
                }
            }
        }
        // Absent arguments reach a tool as null.
        other => issues.push(invalid_type(
            "object",
            Some(other).filter(|value| !value.is_null()),
            &[],
        )),
    }
    if issues.is_empty() {
        return Ok(());
    }
    Err(format!(
        "MCP error -32602: Input validation error: Invalid arguments for tool {}: {}",
        def.name,
        pretty(&Value::Array(issues))
    ))
}

fn invalid_type(expected: &str, value: Option<&Value>, path: &[Value]) -> Value {
    let received = match value {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    };
    json!({
        "expected": expected,
        "code": "invalid_type",
        "path": path,
        "message": format!("Invalid input: expected {expected}, received {received}"),
    })
}

fn check(schema: &Value, value: Option<&Value>, path: &mut Vec<Value>, issues: &mut Vec<Value>) {
    let kind = schema.get("type").and_then(Value::as_str).unwrap_or("");
    match (kind, value) {
        ("string", value) => match schema.get("enum").and_then(Value::as_array) {
            // A missing or mistyped enum value is reported like a wrong one.
            Some(options) => {
                if !value.is_some_and(|value| options.contains(value)) {
                    let listed: Vec<String> = options.iter().map(Value::to_string).collect();
                    issues.push(json!({
                        "code": "invalid_value",
                        "values": options,
                        "path": path,
                        "message": format!("Invalid option: expected one of {}", listed.join("|")),
                    }));
                }
            }
            None => {
                if !value.is_some_and(Value::is_string) {
                    issues.push(invalid_type("string", value, path));
                }
            }
        },
        ("boolean", value) => {
            if !value.is_some_and(Value::is_boolean) {
                issues.push(invalid_type("boolean", value, path));
            }
        }
        ("integer", Some(Value::Number(number))) => {
            check_integer(schema, number.as_f64().unwrap_or(0.0), path, issues);
        }
        ("number" | "integer", value) => {
            if !value.is_some_and(Value::is_number) {
                issues.push(invalid_type("number", value, path));
            }
        }
        ("array", Some(Value::Array(items))) => {
            let item_schema = schema.get("items").unwrap_or(&Value::Null);
            for (index, item) in items.iter().enumerate() {
                path.push(json!(index));
                check(item_schema, Some(item), path, issues);
                path.pop();
            }
            // zod reports the length after the items.
            let maximum = schema.get("maxItems").and_then(Value::as_u64);
            if let Some(maximum) = maximum.filter(|&maximum| items.len() as u64 > maximum) {
                issues.push(json!({
                    "origin": "array",
                    "code": "too_big",
                    "maximum": maximum,
                    "inclusive": true,
                    "path": path,
                    "message": format!("Too big: expected array to have <={maximum} items"),
                }));
            }
        }
        ("array", value) => issues.push(invalid_type("array", value, path)),
        _ => {}
    }
}

/// `z.number().int()` followed by `.positive()` and `.max(n)`, in that order.
fn check_integer(schema: &Value, number: f64, path: &[Value], issues: &mut Vec<Value>) {
    if number.fract() != 0.0 {
        // A fraction stops here: zod does not go on to the range checks.
        issues.push(json!({
            "expected": "int",
            "format": "safeint",
            "code": "invalid_type",
            "path": path,
            "message": "Invalid input: expected int, received number",
        }));
        return;
    }
    let note = "Integers must be within the safe integer range.";
    if number > MAX_SAFE_INTEGER as f64 {
        issues.push(json!({
            "code": "too_big",
            "maximum": MAX_SAFE_INTEGER,
            "note": note,
            "origin": "int",
            "inclusive": true,
            "path": path,
            "message": format!("Too big: expected int to be <={MAX_SAFE_INTEGER}"),
        }));
    }
    if number < -MAX_SAFE_INTEGER as f64 {
        issues.push(json!({
            "code": "too_small",
            "minimum": -MAX_SAFE_INTEGER,
            "note": note,
            "origin": "int",
            "inclusive": true,
            "path": path,
            "message": format!("Too small: expected int to be >=-{MAX_SAFE_INTEGER}"),
        }));
    }
    let bound = |name: &str| schema.get(name).and_then(Value::as_i64);
    if let Some(minimum) = bound("exclusiveMinimum").filter(|&min| number <= min as f64) {
        issues.push(json!({
            "origin": "number",
            "code": "too_small",
            "minimum": minimum,
            "inclusive": false,
            "path": path,
            "message": format!("Too small: expected number to be >{minimum}"),
        }));
    }
    if let Some(maximum) = bound("maximum").filter(|&max| number > max as f64) {
        issues.push(json!({
            "origin": "number",
            "code": "too_big",
            "maximum": maximum,
            "inclusive": true,
            "path": path,
            "message": format!("Too big: expected number to be <={maximum}"),
        }));
    }
}

/// The arguments of a call that passed [`validate`]: a getter only has to tell an
/// absent argument from a given one.
pub(crate) struct Args(pub(crate) Value);

impl Args {
    pub(crate) fn opt_str(&self, key: &str) -> Option<&str> {
        self.0.get(key)?.as_str()
    }

    /// A required string.
    pub(crate) fn str(&self, key: &str) -> &str {
        self.opt_str(key).unwrap_or_default()
    }

    pub(crate) fn opt_bool(&self, key: &str) -> Option<bool> {
        self.0.get(key)?.as_bool()
    }

    pub(crate) fn opt_number(&self, key: &str) -> Option<f64> {
        self.0.get(key)?.as_f64()
    }

    pub(crate) fn opt_strings(&self, key: &str) -> Option<Vec<&str>> {
        let items = self.0.get(key)?.as_array()?;
        Some(items.iter().filter_map(Value::as_str).collect())
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use rever_tools_api::tool::{
        BoxFuture, Browser, Cookie, PageInfo, Tool, ToolContext, unsupported,
    };
    use rever_tools_api::utils::Content;
    use serde_json::{Value, json};

    /// Records every call and answers from canned values.
    #[derive(Default)]
    pub(crate) struct FakeBrowser {
        pub calls: Mutex<Vec<String>>,
        /// Answers of `evaluate`, in call order; `null` once they run out.
        pub evaluations: Mutex<VecDeque<Result<Value, String>>>,
        /// Empty: `page` fails.
        pub url: String,
        pub cookies: Vec<Cookie>,
        /// Set: every cookie call fails with this.
        pub cookie_error: Option<String>,
        pub fail_init_scripts: bool,
    }

    impl FakeBrowser {
        pub(crate) fn answering(
            evaluations: impl IntoIterator<Item = Result<Value, String>>,
        ) -> Self {
            Self {
                evaluations: Mutex::new(evaluations.into_iter().collect()),
                ..Self::default()
            }
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn record(&self, call: String) {
            self.calls.lock().unwrap().push(call);
        }

        fn cookie_result<T>(&self, value: T) -> Result<T, String> {
            match &self.cookie_error {
                Some(error) => Err(error.clone()),
                None => Ok(value),
            }
        }
    }

    impl Browser for FakeBrowser {
        fn page(&self) -> BoxFuture<'_, Result<PageInfo, String>> {
            Box::pin(async {
                if self.url.is_empty() {
                    return Err("no page".to_owned());
                }
                Ok(PageInfo {
                    url: self.url.clone(),
                    title: String::new(),
                })
            })
        }

        fn evaluate<'a>(&'a self, expression: &'a str) -> BoxFuture<'a, Result<Value, String>> {
            Box::pin(async move {
                self.record(format!("evaluate:{expression}"));
                let next = self.evaluations.lock().unwrap().pop_front();
                next.unwrap_or(Ok(Value::Null))
            })
        }

        fn cookies<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Vec<Cookie>, String>> {
            Box::pin(async move {
                self.record(format!("cookies:{url}"));
                self.cookie_result(self.cookies.clone())
            })
        }

        fn set_cookie<'a>(
            &'a self,
            url: &'a str,
            cookie: Cookie,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.record(format!("set_cookie:{url}:{cookie:?}"));
                self.cookie_result(())
            })
        }

        fn delete_cookie<'a>(
            &'a self,
            url: &'a str,
            name: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.record(format!("delete_cookie:{url}:{name}"));
                self.cookie_result(())
            })
        }

        fn add_init_script<'a>(&'a self, source: &'a str) -> BoxFuture<'a, Result<String, String>> {
            Box::pin(async move {
                self.record(format!("add_init_script:{source}"));
                if self.fail_init_scripts {
                    return Err("init scripts are broken".to_owned());
                }
                let added = self
                    .calls()
                    .iter()
                    .filter(|call| call.starts_with("add_init_script:"))
                    .count();
                Ok(format!("script-{added}"))
            })
        }

        // Fails like the real app does today, so tests show the failure is ignored.
        fn remove_init_script<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.record(format!("remove_init_script:{id}"));
                Err(unsupported("removing init scripts"))
            })
        }
    }

    /// A context over `browser` with its own data folder (not created yet).
    pub(crate) fn context(browser: FakeBrowser) -> (Arc<FakeBrowser>, ToolContext) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let data_dir = std::env::temp_dir().join(format!(
            "rever-tools-page-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let browser = Arc::new(browser);
        let context = ToolContext::new(Arc::default(), browser.clone(), data_dir);
        (browser, context)
    }

    /// The result text: `Ok` for a success, `Err` for an `isError` result.
    pub(crate) async fn call(
        tool: &dyn Tool,
        context: &ToolContext,
        args: Value,
    ) -> Result<String, String> {
        let result = tool.call(context, args).await;
        let text = match result.content.into_iter().next() {
            Some(Content::Text { text }) => text,
            None => String::new(),
        };
        if result.is_error { Err(text) } else { Ok(text) }
    }

    /// The result of a call the schema check rejected with these zod issues.
    pub(crate) fn rejected(tool: &str, issues: Value) -> Result<String, String> {
        Err(format!(
            "MCP error -32602: Input validation error: Invalid arguments for tool {tool}: {}",
            serde_json::to_string_pretty(&issues).unwrap()
        ))
    }

    /// zod's issue for `.positive()`.
    pub(crate) fn too_small(key: &str) -> Value {
        json!({
            "origin": "number", "code": "too_small", "minimum": 0, "inclusive": false,
            "path": [key], "message": "Too small: expected number to be >0"
        })
    }

    /// zod's issue for `.max(maximum)`.
    pub(crate) fn too_big(key: &str, maximum: u64) -> Value {
        json!({
            "origin": "number", "code": "too_big", "maximum": maximum, "inclusive": true,
            "path": [key], "message": format!("Too big: expected number to be <={maximum}")
        })
    }

    /// zod's issue for `.int()` on a fraction.
    pub(crate) fn not_int(key: &str) -> Value {
        json!({
            "expected": "int", "format": "safeint", "code": "invalid_type",
            "path": [key], "message": "Invalid input: expected int, received number"
        })
    }

    /// A call rejected for one argument of the wrong type, spelled out as the
    /// TypeScript server prints it.
    pub(crate) fn invalid(
        tool: &str,
        key: &str,
        expected: &str,
        received: &str,
    ) -> Result<String, String> {
        Err(format!(
            "MCP error -32602: Input validation error: Invalid arguments for tool {tool}: [\n  {{\n    \"expected\": \"{expected}\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"{key}\"\n    ],\n    \"message\": \"Invalid input: expected {expected}, received {received}\"\n  }}\n]"
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn 도구_정의는_타입스크립트가_등록하는_정의와_같다() {
        // The fixture is what the TypeScript files register, dumped with bun through
        // the MCP SDK's own zod → JSON Schema conversion.
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../fixtures/ts-tool-defs.json")).unwrap();
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        let defs = registry.defs();
        assert_eq!(defs.len(), expected.len());
        for want in &expected {
            let name = want["name"].as_str().unwrap();
            let def = defs
                .iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("{name} is not registered"));
            assert_eq!(Value::from(def.description.as_str()), want["description"]);
            // As text, so the order of the properties is checked too: the schema
            // check reports problems in that order.
            assert_eq!(
                def.input_schema.to_string(),
                want["inputSchema"].to_string(),
                "{name}"
            );
        }
    }

    #[test]
    fn 도구는_서버가_등록하는_순서대로_등록된다() {
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        let names: Vec<String> = registry.defs().into_iter().map(|def| def.name).collect();
        assert_eq!(names.first().map(String::as_str), Some("inject_add"));
        assert_eq!(names.last().map(String::as_str), Some("vision_judge"));
        assert_eq!(names.len(), 33);
    }

    #[tokio::test]
    async fn 인자가_아예_없으면_스키마가_있는_도구만_거절한다() {
        use crate::test_support::{FakeBrowser, call, context, rejected};

        let (_, ctx) = context(FakeBrowser::default());
        // Every argument of list_downloads is optional, and the SDK still wants an object.
        assert_eq!(
            call(&downloads::ListDownloads, &ctx, Value::Null).await,
            rejected(
                "list_downloads",
                json!([{
                    "expected": "object", "code": "invalid_type", "path": [],
                    "message": "Invalid input: expected object, received undefined"
                }])
            )
        );
        // A tool registered without a schema never has its arguments looked at.
        for args in [Value::Null, json!({ "extra": 1 }), json!("text")] {
            assert_eq!(
                call(&r#override::OverrideList, &ctx, args).await,
                Ok("[]".to_owned())
            );
        }
        // An unknown key is dropped, not rejected.
        assert_eq!(
            call(
                &r#override::OverrideRemove,
                &ctx,
                json!({ "id": "x", "extra": 1 })
            )
            .await,
            Err("not an override: x".to_owned())
        );
    }

    #[test]
    fn js_str은_자바스크립트_문자열_리터럴을_만든다() {
        assert_eq!(js_str("a\"b\\c\n</x>한"), r#""a\"b\\c\n</x>한""#);
    }
}
