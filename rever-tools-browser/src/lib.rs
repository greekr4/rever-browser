//! MCP tools driving the tab: navigate, snapshot, click, type, scroll,
//! screenshot, waits, UI crawl.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rever_tools_api::tool::{Browser, ToolContext, ToolDef, ToolRegistry, unsupported};
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

pub mod bridge;
pub mod browser;
pub mod cdp_eval;
pub mod gestures;
pub mod human_input;
pub mod ui_crawl;
pub mod wait;

/// Registers every tool of this crate, in the order `buildMcpServer` does.
pub fn register(registry: &mut ToolRegistry) {
    browser::register(registry);
    ui_crawl::register(registry);
    wait::register(registry);
}

pub(crate) fn done(result: Result<String, String>) -> ToolResult {
    match result {
        Ok(text) => ok(text),
        Err(text) => err(text),
    }
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

/// The optional `tab` argument, as zod's `z.number().int().optional()` lists it.
pub(crate) fn tab_schema(description: &str) -> Value {
    json!({
        "description": description,
        "type": "integer",
        "minimum": -MAX_SAFE_INTEGER,
        "maximum": MAX_SAFE_INTEGER,
    })
}

/// `Browser` addresses the one tab the agent works in, so a call that names
/// another tab by index cannot be routed. Used by the snapshot/interaction
/// tools, whose ref map is still per-process (a background tab needs a per-tab
/// snapshotter — see docs/port-progress.md A).
pub(crate) fn no_tab(args: &Args) -> Result<(), String> {
    match args.0.get("tab") {
        Some(_) => Err(unsupported("driving a tab by index")),
        None => Ok(()),
    }
}

/// The browser handle a tool should drive: the tab the `tab` arg names (via
/// `for_tab`, without switching the user's tab), or the active tab when absent.
/// For the snapshotter-free tools (evaluate, screenshot).
pub(crate) fn tab_browser(ctx: &ToolContext, args: &Args) -> Result<Arc<dyn Browser>, String> {
    match args.0.get("tab").and_then(Value::as_i64) {
        None => Ok(ctx.browser.clone()),
        Some(index) if index >= 0 => ctx
            .browser
            .for_tab(index as usize)
            .ok_or_else(|| unsupported("driving a tab by index")),
        Some(_) => Err("tab index must be >= 0".to_owned()),
    }
}

/// A panic in another call must not take the tool state down with it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `Date.now()`.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// `setTimeout`-style pause in milliseconds; a negative or NaN time does not wait.
pub(crate) async fn sleep_ms(ms: f64) {
    if ms > 0.0 {
        tokio::time::sleep(Duration::from_secs_f64(ms / 1000.0)).await;
    }
}

/// `Math.round`: halves go up, also below zero.
pub(crate) fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

/// JavaScript truthiness.
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|f| f != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// What JavaScript's `\s` and `String.prototype.trim` call whitespace: Rust's set
/// plus U+FEFF, minus U+0085.
pub(crate) fn js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

/// `text.slice(0, units)` counted in UTF-16 units like JavaScript. A cut that
/// would split a surrogate pair stops before it: a Rust string cannot hold half.
pub(crate) fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut seen = 0;
    for (byte, ch) in text.char_indices() {
        seen += ch.len_utf16();
        if seen > units {
            return text.split_at_checked(byte).map_or(text, |(head, _)| head);
        }
    }
    text
}

pub(crate) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// `JSON.stringify(text)`: a string as a JavaScript string literal.
pub(crate) fn js_str(text: &str) -> String {
    Value::from(text).to_string()
}

/// `JSON.stringify(value, null, 2)`.
pub(crate) fn pretty(value: &Value) -> String {
    let mut out = String::new();
    write_js(value, 0, &mut out);
    out
}

fn write_js(value: &Value, depth: usize, out: &mut String) {
    fn line(out: &mut String, depth: usize) {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    }
    match value {
        // Every JavaScript number is a double: 1.0 prints as 1, and an integer past
        // the safe range prints the digits of the double it became.
        Value::Number(number) => match number
            .as_i64()
            .filter(|integer| integer.unsigned_abs() <= MAX_SAFE_INTEGER.unsigned_abs())
        {
            Some(integer) => out.push_str(&integer.to_string()),
            None => out.push_str(&js_number(number.as_f64().unwrap_or_default())),
        },
        Value::Array(items) if !items.is_empty() => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                line(out, depth + 1);
                write_js(item, depth + 1, out);
            }
            line(out, depth);
            out.push(']');
        }
        Value::Object(fields) if !fields.is_empty() => {
            out.push('{');
            for (index, (key, field)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                line(out, depth + 1);
                out.push_str(&json!(key).to_string());
                out.push_str(": ");
                write_js(field, depth + 1, out);
            }
            line(out, depth);
            out.push('}');
        }
        // null, booleans, strings, and the empty [] and {}.
        other => out.push_str(&other.to_string()),
    }
}

/// `String(number)` in JavaScript: the shortest digits that read back as the same
/// double, laid out by the rules of Number::toString.
pub(crate) fn js_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if value == 0.0 {
        // -0 prints as 0 too.
        return "0".to_owned();
    }
    // Rust's exponent form carries the same shortest digits: "1.2345e-7".
    let formatted = format!("{:e}", value.abs());
    let (mantissa, exponent) = formatted.split_once('e').unwrap_or((&formatted, "0"));
    let digits = mantissa.replace('.', "");
    let count = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    // How many digits stand before the decimal point.
    let point = exponent.parse::<i32>().unwrap_or(0) + 1;
    let zeros = |n: i32| "0".repeat(usize::try_from(n).unwrap_or(0));
    let split = |at: i32| {
        usize::try_from(at)
            .ok()
            .and_then(|at| digits.split_at_checked(at))
            .unwrap_or((&digits, ""))
    };
    let body = if (count..=21).contains(&point) {
        format!("{digits}{}", zeros(point - count))
    } else if (1..=21).contains(&point) {
        let (whole, fraction) = split(point);
        format!("{whole}.{fraction}")
    } else if (-5..=0).contains(&point) {
        format!("0.{}{digits}", zeros(-point))
    } else {
        let (first, rest) = split(1);
        let dot = if rest.is_empty() { "" } else { "." };
        let sign = if point > 0 { '+' } else { '-' };
        format!("{first}{dot}{rest}e{sign}{}", (point - 1).abs())
    };
    if value < 0.0 {
        format!("-{body}")
    } else {
        body
    }
}

// Number.MAX_SAFE_INTEGER: zod's `.int()` only accepts integers a double holds exactly.
pub(crate) const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Checks the arguments against the tool's own schema the way the MCP SDK does
/// before a handler runs: every problem at once, in the order of the schema, in the
/// words zod uses, because that text is what the agent reads and corrects from.
/// `Err` carries the text of the `isError` result. It knows the constructs these
/// tools use: string, enum, boolean, number, bounded integer, array and object.
pub(crate) fn validate(def: &ToolDef, args: &Value) -> Result<(), String> {
    let mut issues = Vec::new();
    // Absent arguments reach a tool as null, which zod saw as undefined.
    let given = Some(args).filter(|value| !value.is_null());
    check_object(&def.input_schema, given, &mut Vec::new(), &mut issues);
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

fn check_object(
    schema: &Value,
    value: Option<&Value>,
    path: &mut Vec<Value>,
    issues: &mut Vec<Value>,
) {
    let Some(Value::Object(fields)) = value else {
        issues.push(invalid_type("object", value, path));
        return;
    };
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    let required = schema.get("required").and_then(Value::as_array);
    for (key, property) in properties {
        let field = fields.get(key);
        let is_required =
            required.is_some_and(|names| names.iter().any(|name| name.as_str() == Some(key)));
        // Optional means absent; null is a value of the wrong type.
        if field.is_some() || is_required {
            path.push(json!(key));
            check(property, field, path, issues);
            path.pop();
        }
    }
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
        }
        ("array", value) => issues.push(invalid_type("array", value, path)),
        ("object", value) => check_object(schema, value, path, issues),
        _ => {}
    }
}

/// `z.number().int()` followed by `.min(n)` / `.positive()` and `.max(n)`, in that order.
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
    // A bare `.int()` lists the safe range as its bounds; those are reported above.
    let bound = |name: &str| {
        let bound = schema.get(name).and_then(Value::as_i64);
        bound.filter(|bound| bound.abs() != MAX_SAFE_INTEGER)
    };
    if let Some(minimum) = bound("minimum").filter(|&min| number < min as f64) {
        issues.push(json!({
            "origin": "number",
            "code": "too_small",
            "minimum": minimum,
            "inclusive": true,
            "path": path,
            "message": format!("Too small: expected number to be >={minimum}"),
        }));
    }
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

    use rever_tools_api::tool::{BoxFuture, Browser, Input, PageInfo, TabInfo, Tool, ToolContext};
    use rever_tools_api::utils::Content;
    use serde_json::{Value, json};

    /// Records every call and answers from canned values.
    #[derive(Default)]
    pub(crate) struct FakeBrowser {
        /// Everything but the overlay calls, in order.
        pub calls: Mutex<Vec<String>>,
        /// `window.__reverAi` calls: cosmetic, so they never take a canned answer.
        pub overlay: Mutex<Vec<String>>,
        pub inputs: Mutex<Vec<Input>>,
        pub tabs: Mutex<Vec<TabInfo>>,
        /// Answers of plain `evaluate` calls, in order; `null` once they run out.
        pub evaluations: Mutex<VecDeque<Result<Value, String>>>,
        /// Answers of the snapshot script's `act`, in order; a default once they run out.
        pub actions: Mutex<VecDeque<Value>>,
        /// What the snapshot script's `capture` reports; `None` makes it fail.
        pub capture: Mutex<Option<Value>>,
        pub url: Mutex<String>,
        pub fail_input: bool,
    }

    /// A page with one button, which a snapshot hands the ref `r1`.
    pub(crate) fn one_button() -> Value {
        json!({
            "meta": { "url": "https://a.test/", "title": "A" },
            "nodes": [
                { "nodeId": "1", "role": { "value": "RootWebArea" }, "name": { "value": "A" }, "childIds": ["2"] },
                { "nodeId": "2", "role": { "value": "button" }, "name": { "value": "Save" }, "backendDOMNodeId": 10 }
            ]
        })
    }

    impl FakeBrowser {
        pub(crate) fn new() -> Self {
            Self {
                capture: Mutex::new(Some(one_button())),
                url: Mutex::new("https://a.test/".to_owned()),
                ..Self::default()
            }
        }

        pub(crate) fn answering(
            evaluations: impl IntoIterator<Item = Result<Value, String>>,
        ) -> Self {
            let browser = Self::new();
            *browser.evaluations.lock().unwrap() = evaluations.into_iter().collect();
            browser
        }

        pub(crate) fn with_actions(self, actions: impl IntoIterator<Item = Value>) -> Self {
            *self.actions.lock().unwrap() = actions.into_iter().collect();
            self
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        pub(crate) fn overlay(&self) -> Vec<String> {
            self.overlay.lock().unwrap().clone()
        }

        pub(crate) fn inputs(&self) -> Vec<Input> {
            self.inputs.lock().unwrap().clone()
        }

        fn record(&self, call: String) {
            self.calls.lock().unwrap().push(call);
        }
    }

    impl Browser for FakeBrowser {
        fn page(&self) -> BoxFuture<'_, Result<PageInfo, String>> {
            Box::pin(async {
                Ok(PageInfo {
                    url: self.url.lock().unwrap().clone(),
                    title: String::new(),
                })
            })
        }

        fn navigate<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.record(format!("navigate:{url}"));
                *self.url.lock().unwrap() = url.to_owned();
                Ok(())
            })
        }

        fn back(&self) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async {
                self.record("back".to_owned());
                Ok(())
            })
        }

        fn forward(&self) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async {
                self.record("forward".to_owned());
                Ok(())
            })
        }

        fn evaluate<'a>(&'a self, expression: &'a str) -> BoxFuture<'a, Result<Value, String>> {
            Box::pin(async move {
                // The snapshot script is called as `(<source>)("command", argument)`.
                if expression.starts_with("(// Page-side half of the snapshot") {
                    let call = expression
                        .rfind("\n)(")
                        .map_or("", |at| &expression[at + 2..]);
                    self.record(format!("script:{call}"));
                    if call.starts_with("(\"capture\"") {
                        return self
                            .capture
                            .lock()
                            .unwrap()
                            .clone()
                            .ok_or("no page".to_owned());
                    }
                    if call.starts_with("(\"setRefs\"") {
                        return Ok(json!(0));
                    }
                    if let Some(answer) = self.actions.lock().unwrap().pop_front() {
                        return Ok(answer);
                    }
                    return Ok(if call.contains("\"action\":\"point\"") {
                        json!({ "ok": { "x": 100, "y": 50, "left": 80, "top": 40, "width": 40, "height": 20 } })
                    } else {
                        json!({ "ok": true })
                    });
                }
                if expression.contains("__reverAi") {
                    self.overlay.lock().unwrap().push(expression.to_owned());
                    return Ok(Value::Null);
                }
                self.record(format!("evaluate:{expression}"));
                let next = self.evaluations.lock().unwrap().pop_front();
                next.unwrap_or(Ok(Value::Null))
            })
        }

        fn screenshot(&self) -> BoxFuture<'_, Result<Vec<u8>, String>> {
            Box::pin(async { Ok(vec![0x89, b'P', b'N', b'G']) })
        }

        fn tabs(&self) -> BoxFuture<'_, Result<Vec<TabInfo>, String>> {
            Box::pin(async { Ok(self.tabs.lock().unwrap().clone()) })
        }

        fn select_tab(&self, index: usize) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async move {
                for (i, tab) in self.tabs.lock().unwrap().iter_mut().enumerate() {
                    tab.active = i == index;
                }
                Ok(())
            })
        }

        fn open_tab<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<usize, String>> {
            Box::pin(async move {
                let mut tabs = self.tabs.lock().unwrap();
                tabs.push(TabInfo {
                    url: url.to_owned(),
                    ..TabInfo::default()
                });
                Ok(tabs.len() - 1)
            })
        }

        fn input(&self, input: Input) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async move {
                if self.fail_input {
                    return Err("input is broken".to_owned());
                }
                self.inputs.lock().unwrap().push(input);
                Ok(())
            })
        }
    }

    pub(crate) fn context(browser: FakeBrowser) -> (Arc<FakeBrowser>, ToolContext) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let data_dir = std::env::temp_dir().join(format!(
            "rever-tools-browser-test-{}-{}",
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

    /// The scope and the fence are process-wide in rever-scope, so the tests that
    /// set them, and the ones whose result depends on them, take turns.
    pub(crate) static SCOPE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Let the spawned overlay calls run.
    pub(crate) async fn settle_overlay() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::test_support::{FakeBrowser, call, context};

    fn registry() -> ToolRegistry {
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        registry
    }

    #[test]
    fn 도구_정의는_타입스크립트가_등록하는_정의와_같다() {
        // The fixture is what browser.ts, ui-crawl.ts and wait.ts register, dumped
        // with bun through the MCP SDK's own zod → JSON Schema conversion.
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../fixtures/ts-tool-defs.json")).unwrap();
        let defs = registry().defs();
        assert_eq!(defs.len(), expected.len());
        // In the same order too: the fixture was dumped in registration order.
        for (def, want) in defs.iter().zip(&expected) {
            let name = want["name"].as_str().unwrap();
            assert_eq!(def.name, name);
            assert_eq!(
                Value::from(def.description.as_str()),
                want["description"],
                "{name}"
            );
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
    fn 도구는_서버가_등록하는_순서대로_26개_등록된다() {
        let names: Vec<String> = registry().defs().into_iter().map(|def| def.name).collect();
        assert_eq!(names.first().map(String::as_str), Some("browser_navigate"));
        assert_eq!(names.get(24).map(String::as_str), Some("ui_crawl"));
        assert_eq!(names.last().map(String::as_str), Some("browser_wait_for"));
        assert_eq!(names.len(), 26);
    }

    #[tokio::test]
    async fn 잘못된_인자는_zod와_같은_글로_페이지에_닿기_전에_거절한다() {
        // The fixture is what zod answers for these arguments, dumped with bun
        // through the MCP SDK's own parse and error formatting.
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("../fixtures/ts-validation.json")).unwrap();
        assert!(cases.len() > 20);
        let registry = registry();
        let (browser, ctx) = context(FakeBrowser::new());
        for case in &cases {
            let name = case["name"].as_str().unwrap();
            let tool = registry.find(name).unwrap();
            assert_eq!(
                call(tool.as_ref(), &ctx, case["args"].clone()).await,
                Err(case["text"].as_str().unwrap().to_owned()),
                "{name} {}",
                case["args"]
            );
        }
        assert!(browser.calls().is_empty());
        assert!(browser.inputs().is_empty());
    }

    #[test]
    fn js_number는_number_to_string과_같은_글자를_낸다() {
        // Expected strings are what String(n) prints in bun.
        for (number, text) in [
            (0.0, "0"),
            (-0.0, "0"),
            (3.0, "3"),
            (-2.5, "-2.5"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e21, "1e+21"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
        ] {
            assert_eq!(js_number(number), text);
        }
    }

    #[test]
    fn pretty는_json_stringify의_두_칸_들여쓰기와_같은_모양이다() {
        assert_eq!(pretty(&json!({})), "{}");
        assert_eq!(
            pretty(&json!({ "a": [1, 2.0, 0.5], "b": {}, "c": "한\"\n", "d": [] })),
            "{\n  \"a\": [\n    1,\n    2,\n    0.5\n  ],\n  \"b\": {},\n  \"c\": \"한\\\"\\n\",\n  \"d\": []\n}"
        );
    }

    #[test]
    fn 글자_수는_utf16_단위로_세고_서로게이트_쌍은_쪼개지_않는다() {
        assert_eq!(utf16_prefix("가나다라", 2), "가나");
        assert_eq!(utf16_prefix("ab", 5), "ab");
        // The emoji is two units; a cut inside it stops before it.
        assert_eq!(utf16_prefix("a😀b", 2), "a");
        assert_eq!(utf16_prefix("a😀b", 3), "a😀");
        assert_eq!(utf16_len("a😀b"), 4);
    }

    #[test]
    fn 반올림은_자바스크립트처럼_절반을_위로_올린다() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(2.4), 2.0);
    }

    #[test]
    fn 공백과_참거짓은_자바스크립트의_기준을_따른다() {
        assert!(js_space('\u{feff}') && js_space('\u{a0}') && js_space('\n'));
        assert!(!js_space('\u{85}') && !js_space('a'));
        assert!(!truthy(&json!(0)) && !truthy(&json!("")) && !truthy(&Value::Null));
        assert!(truthy(&json!([])) && truthy(&json!("0")) && truthy(&json!(-1)));
    }
}
