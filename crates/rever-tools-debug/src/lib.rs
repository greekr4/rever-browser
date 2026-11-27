//! MCP tools for the JavaScript debugger and request interception.
//!
//! Both tool families drive Chrome over the DevTools Protocol in the TypeScript.
//! This app's engine does not expose a debugger or request interception yet, so
//! the calls that need it go through [`debugger::DebugEngine`] and
//! [`intercept::InterceptEngine`]; until the app plugs real ones in
//! (`ctx.state::<DebugState>().set_engine(…)`), those calls answer "not supported".

use std::sync::{Mutex, MutexGuard};

use rever_tools_api::tool::ToolRegistry;
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Value, json};

/// Declares a unit-struct tool: its definition, and a body that yields the result
/// text as `Result<String, String>` (`Err` becomes an `isError` result). The body is
/// an async block, so `?` works in it; `$args` is the arguments object, already
/// checked against the tool's schema.
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
                    let args = $crate::js_key_order(args);
                    let $args = &args;
                    let result: Result<String, String> = async {
                        $crate::args::validate(&self.def(), $args)?;
                        $body
                    }
                    .await;
                    $crate::done(result)
                })
            }
        }
    };
}

pub mod args;
pub mod debugger;
pub mod intercept;

/// Registers every tool of this crate, in the order `buildMcpServer` does.
pub fn register(registry: &mut ToolRegistry) {
    intercept::register(registry);
    debugger::register(registry);
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
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "$schema": "http://json-schema.org/draft-07/schema#",
    })
}

/// A JSON value as JavaScript holds it once parsed: an object lists its
/// integer-like keys first, in ascending order, then the rest in the order given.
/// Both the arguments (zod reports unknown keys in that order) and everything the
/// TypeScript printed went through such objects.
pub(crate) fn js_key_order(value: Value) -> Value {
    match value {
        Value::Array(items) => items.into_iter().map(js_key_order).collect(),
        Value::Object(fields) => {
            // An array index: canonical decimal below 2^32 - 1 ("01" and "-1" are not).
            let index = |key: &str| {
                let number = key.parse::<u32>().ok()?;
                (number != u32::MAX && number.to_string() == key).then_some(number)
            };
            let mut entries: Vec<(String, Value)> = fields
                .into_iter()
                .map(|(key, value)| (key, js_key_order(value)))
                .collect();
            // Stable, so the other keys keep the order they came in.
            entries.sort_by_key(|(key, _)| index(key).map_or((1, 0), |number| (0, number)));
            Value::Object(entries.into_iter().collect())
        }
        other => other,
    }
}

/// `JSON.stringify(value, null, 2)`.
pub(crate) fn pretty(value: &impl serde::Serialize) -> String {
    let value = js_key_order(serde_json::to_value(value).unwrap_or_default());
    let mut out = String::new();
    write_js(&value, 0, &mut out);
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
            .filter(|integer| integer.unsigned_abs() <= args::MAX_SAFE_INTEGER.unsigned_abs())
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
fn js_number(value: f64) -> String {
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

/// A panic in another call must not take the tool state down with it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
pub(crate) mod test_support {
    use rever_tools_api::tool::{Tool, ToolContext};
    use rever_tools_api::utils::Content;
    use serde_json::Value;

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

    /// A successful result parsed as JSON.
    pub(crate) async fn call_json(tool: &dyn Tool, context: &ToolContext, args: Value) -> Value {
        let text = call(tool, context, args).await.expect("tool failed");
        serde_json::from_str(&text).expect("result is not JSON")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tools whose description this build rewrites. The fixture keeps what
    /// `debugger.ts` registered; these six described Electron's debugger and are
    /// wrong here (a breakpoint freezes the tab).
    const REWRITTEN_DESCRIPTIONS: [&str; 6] = [
        "bp_add",
        "bp_eval_in_frame",
        "bp_resume",
        "bp_step_into",
        "bp_step_out",
        "bp_step_over",
    ];

    #[test]
    fn 도구_정의는_타입스크립트가_등록하는_정의와_같다() {
        // The fixture is what debugger.ts and intercept.ts register, dumped with bun
        // through the MCP SDK's own zod → JSON Schema conversion (tools/list).
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../fixtures/ts-tool-defs.json")).unwrap();
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        let defs = registry.defs();
        let names: Vec<&str> = defs.iter().map(|def| def.name.as_str()).collect();
        let expected_names: Vec<&str> = expected
            .iter()
            .map(|want| want["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, expected_names);
        for (def, want) in defs.iter().zip(&expected) {
            if REWRITTEN_DESCRIPTIONS.contains(&def.name.as_str()) {
                // The TypeScript text explains Electron's debugger, which is not
                // what this build does; it must not be shipped again.
                assert!(!def.description.contains("Electron"), "{}", def.name);
            } else {
                assert_eq!(Value::from(def.description.as_str()), want["description"]);
            }
            assert_eq!(def.input_schema, want["inputSchema"], "{}", def.name);
            // Key order too: a client shows the properties in this order.
            assert_eq!(
                def.input_schema.to_string(),
                want["inputSchema"].to_string(),
                "{}",
                def.name
            );
        }
    }

    #[test]
    fn 도구는_서버가_등록하는_순서대로_17개_등록된다() {
        let mut registry = ToolRegistry::default();
        register(&mut registry);
        let names: Vec<String> = registry.defs().into_iter().map(|def| def.name).collect();
        assert_eq!(names.first().map(String::as_str), Some("intercept_add"));
        assert_eq!(names.get(8).map(String::as_str), Some("bp_add"));
        assert_eq!(names.last().map(String::as_str), Some("bp_eval_in_frame"));
        assert_eq!(names.len(), 17);
    }

    #[test]
    fn pretty는_json_stringify의_두_칸_들여쓰기와_같은_모양이다() {
        // Expected strings are what JSON.stringify(v, null, 2) prints in bun.
        assert_eq!(
            pretty(&json!({ "paused": false })),
            "{\n  \"paused\": false\n}"
        );
        assert_eq!(pretty(&json!([])), "[]");
        assert_eq!(pretty(&json!("desc")), "\"desc\"");
        assert_eq!(pretty(&Value::Null), "null");
        assert_eq!(
            pretty(&json!({ "a": [], "b": {} })),
            "{\n  \"a\": [],\n  \"b\": {}\n}"
        );
        assert_eq!(
            pretty(&json!([[1], "a\"\n\u{1}é"])),
            "[\n  [\n    1\n  ],\n  \"a\\\"\\n\\u0001é\"\n]"
        );
    }

    #[test]
    fn pretty는_수를_자바스크립트처럼_출력한다() {
        // What bp_eval_in_frame answered in the TypeScript for this value (bun).
        let value = json!({
            "a": [1.0, 1e21, 1e16, 0.000001, 1.5e-7, -0.0, 1e300, 2.5, [], {}],
            "n": 1152921504606846976u64
        });
        assert_eq!(
            pretty(&value),
            "{\n  \"a\": [\n    1,\n    1e+21,\n    10000000000000000,\n    0.000001,\n    1.5e-7,\n    0,\n    1e+300,\n    2.5,\n    [],\n    {}\n  ],\n  \"n\": 1152921504606847000\n}"
        );
    }

    #[test]
    fn js_number는_number_to_string과_같은_글자를_낸다() {
        // Expected strings are String(n) in Node for the same doubles.
        for (number, text) in [
            (1.0, "1"),
            (1.5, "1.5"),
            (-0.0, "0"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123456789012345680000.0, "123456789012345680000"),
            (1152921504606846976.0, "1152921504606847000"),
            (1e16, "10000000000000000"),
            (9007199254740993.0, "9007199254740992"),
            (0.000001, "0.000001"),
            (0.0000012345, "0.0000012345"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (123e-20, "1.23e-18"),
            (0.1, "0.1"),
            (1e300, "1e+300"),
            (-1e21, "-1e+21"),
            (-2.5e-9, "-2.5e-9"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (100.0, "100"),
            (1234.5678, "1234.5678"),
            (4.35, "4.35"),
            (0.30000000000000004, "0.30000000000000004"),
            (1e15, "1000000000000000"),
            (123456789.123, "123456789.123"),
        ] {
            assert_eq!(js_number(number), text);
        }
    }

    #[test]
    fn 객체의_정수_모양_키는_자바스크립트처럼_오름차순으로_앞에_온다() {
        // JSON.stringify(JSON.parse(text)) in Node for the same text.
        let value: Value = serde_json::from_str(
            r#"{"b":1,"2":"x","a":{"10":1,"9":2,"01":3,"-1":4,"4294967294":5,"4294967295":6,"1.5":7,"z":8},"1":"y"}"#,
        )
        .unwrap();
        assert_eq!(
            js_key_order(value).to_string(),
            r#"{"1":"y","2":"x","b":1,"a":{"9":2,"10":1,"4294967294":5,"01":3,"-1":4,"4294967295":6,"1.5":7,"z":8}}"#
        );
        // Inside arrays too.
        assert_eq!(
            js_key_order(json!([{ "b": 1, "0": 2 }])).to_string(),
            r#"[{"0":2,"b":1}]"#
        );
    }
}
