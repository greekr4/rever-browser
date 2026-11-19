//! Argument validation. The TypeScript declared each tool's arguments with zod
//! and the MCP SDK rejected a bad call before the handler ran; here the same
//! checks are read off the tool's JSON Schema, and the error text is the one
//! the SDK returned (zod's issue list), so an agent sees the same correction.

use rever_tools_api::tool::ToolDef;
use rever_tools_api::utils::{ToolResult, err};
use serde_json::{Value, json};

use crate::js::json_stringify;

/// Number.MAX_SAFE_INTEGER, the bound zod's `.int()` enforces.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Checks `args` against the tool's input schema. `Err` is the finished tool
/// result to return.
pub(crate) fn validate(def: &ToolDef, args: &Value) -> Result<(), ToolResult> {
    let mut issues = Vec::new();
    match args {
        Value::Object(map) => {
            let schema = &def.input_schema;
            let required = schema.get("required").and_then(Value::as_array);
            let properties = schema.get("properties").and_then(Value::as_object);
            for (name, property) in properties.into_iter().flatten() {
                let value = map.get(name);
                let is_required =
                    required.is_some_and(|names| names.iter().any(|n| n.as_str() == Some(name)));
                if value.is_some() || is_required {
                    check(property, value, vec![json!(name)], &mut issues);
                }
            }
        }
        // A call without arguments reaches the tool as null.
        Value::Null => issues.push(invalid_type("object", None, vec![])),
        other => issues.push(invalid_type("object", Some(other), vec![])),
    }
    if issues.is_empty() {
        return Ok(());
    }
    Err(err(format!(
        "MCP error -32602: Input validation error: Invalid arguments for tool {}: {}",
        def.name,
        json_stringify(&Value::Array(issues), true)
    )))
}

/// zod's name for the type of a value; `None` is `undefined`.
fn received(value: Option<&Value>) -> &'static str {
    match value {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

fn invalid_type(expected: &str, value: Option<&Value>, path: Vec<Value>) -> Value {
    json!({
        "expected": expected,
        "code": "invalid_type",
        "path": path,
        "message": format!("Invalid input: expected {expected}, received {}", received(value)),
    })
}

fn check(schema: &Value, value: Option<&Value>, path: Vec<Value>, issues: &mut Vec<Value>) {
    let kind = schema.get("type").and_then(Value::as_str).unwrap_or("");
    match (kind, value) {
        ("string", _) if schema.get("enum").is_some() => {
            let values = schema.get("enum").and_then(Value::as_array);
            let allowed = values.into_iter().flatten();
            if !value.is_some_and(|v| v.is_string() && allowed.clone().any(|a| a == v)) {
                let options: Vec<String> = allowed.clone().map(Value::to_string).collect();
                issues.push(json!({
                    "code": "invalid_value",
                    "values": values,
                    "path": path,
                    "message": format!("Invalid option: expected one of {}", options.join("|")),
                }));
            }
        }
        ("string", Some(Value::String(_))) | ("boolean", Some(Value::Bool(_))) => {}
        ("integer", Some(Value::Number(n))) => {
            check_integer(schema, n.as_f64().unwrap_or(0.0), path, issues);
        }
        ("integer", _) => issues.push(invalid_type("number", value, path)),
        // The only object arguments are string records (`z.record`).
        ("object", Some(Value::Object(map))) => {
            if let Some(item) = schema.get("additionalProperties") {
                for (key, entry) in map {
                    let mut entry_path = path.clone();
                    entry_path.push(json!(key));
                    check(item, Some(entry), entry_path, issues);
                }
            }
        }
        ("object", _) => issues.push(invalid_type("record", value, path)),
        ("array", Some(Value::Array(items))) => {
            if let Some(item) = schema.get("items") {
                for (index, entry) in items.iter().enumerate() {
                    let mut entry_path = path.clone();
                    entry_path.push(json!(index));
                    check(item, Some(entry), entry_path, issues);
                }
            }
        }
        _ => issues.push(invalid_type(kind, value, path)),
    }
}

/// `z.number().int()` with the bounds the schema carries.
fn check_integer(schema: &Value, n: f64, path: Vec<Value>, issues: &mut Vec<Value>) {
    if n.fract() != 0.0 {
        // zod stops at this one: the bounds are not checked for a non-integer.
        issues.push(json!({
            "expected": "int",
            "format": "safeint",
            "code": "invalid_type",
            "path": path,
            "message": "Invalid input: expected int, received number",
        }));
        return;
    }
    if n > MAX_SAFE_INTEGER {
        issues.push(json!({
            "code": "too_big",
            "maximum": MAX_SAFE_INTEGER,
            "note": "Integers must be within the safe integer range.",
            "origin": "int",
            "inclusive": true,
            "path": path,
            "message": "Too big: expected int to be <=9007199254740991",
        }));
    }
    if n < -MAX_SAFE_INTEGER {
        issues.push(json!({
            "code": "too_small",
            "minimum": -MAX_SAFE_INTEGER,
            "note": "Integers must be within the safe integer range.",
            "origin": "int",
            "inclusive": true,
            "path": path,
            "message": "Too small: expected int to be >=-9007199254740991",
        }));
    }
    let bound = |name: &str| schema.get(name).and_then(Value::as_f64);
    if let Some(minimum) = bound("exclusiveMinimum").filter(|minimum| n <= *minimum) {
        issues.push(json!({
            "origin": "number",
            "code": "too_small",
            "minimum": minimum,
            "inclusive": false,
            "path": path,
            "message": format!("Too small: expected number to be >{}", crate::js::js_number(minimum)),
        }));
    }
    if let Some(maximum) = bound("maximum").filter(|maximum| n > *maximum) {
        issues.push(json!({
            "origin": "number",
            "code": "too_big",
            "maximum": maximum,
            "inclusive": true,
            "path": path,
            "message": format!("Too big: expected number to be <={}", crate::js::js_number(maximum)),
        }));
    }
}

#[cfg(test)]
mod tests {
    use rever_tools_api::utils::Content;

    use super::*;

    fn def() -> ToolDef {
        ToolDef::new(
            "sample",
            "a tool with one argument of every kind this crate uses",
            json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string" },
                    "algorithm": { "type": "string", "enum": ["md5", "sha1"] },
                    "iterations": { "type": "integer", "exclusiveMinimum": 0, "maximum": 100000 },
                    "headers": { "type": "object", "additionalProperties": { "type": "string" } },
                    "transforms": { "type": "array", "items": { "type": "string", "enum": ["a", "b"] } },
                    "flag": { "type": "boolean" }
                },
                "required": ["input", "algorithm", "iterations"]
            }),
        )
    }

    /// The issue list of a rejected call, parsed back.
    fn issues(args: Value) -> Value {
        let Err(result) = validate(&def(), &args) else {
            return json!([]);
        };
        assert!(result.is_error);
        let Some(Content::Text { text }) = result.content.first() else {
            return Value::Null;
        };
        let list = text
            .strip_prefix(
                "MCP error -32602: Input validation error: Invalid arguments for tool sample: ",
            )
            .unwrap_or("null");
        serde_json::from_str(list).unwrap_or(Value::Null)
    }

    fn valid() -> Value {
        json!({ "input": "a", "algorithm": "md5", "iterations": 1 })
    }

    fn with(key: &str, value: Value) -> Value {
        let mut args = valid();
        args[key] = value;
        args
    }

    #[test]
    fn 올바른_인자는_통과하고_모르는_키는_무시한다() {
        assert!(validate(&def(), &valid()).is_ok());
        assert!(validate(&def(), &with("extra", json!(1))).is_ok());
        let all = json!({
            "input": "", "algorithm": "sha1", "iterations": 100000,
            "headers": { "a": "b" }, "transforms": ["a", "b"], "flag": false
        });
        assert!(validate(&def(), &all).is_ok());
    }

    #[test]
    fn 필수_인자가_없으면_zod와_같은_목록을_돌려준다() {
        // The text is what the MCP SDK returned for `{}` with this schema.
        let Err(result) = validate(&def(), &json!({})) else {
            panic!("an empty object must be rejected");
        };
        let expected = r#"MCP error -32602: Input validation error: Invalid arguments for tool sample: [
  {
    "expected": "string",
    "code": "invalid_type",
    "path": [
      "input"
    ],
    "message": "Invalid input: expected string, received undefined"
  },
  {
    "code": "invalid_value",
    "values": [
      "md5",
      "sha1"
    ],
    "path": [
      "algorithm"
    ],
    "message": "Invalid option: expected one of \"md5\"|\"sha1\""
  },
  {
    "expected": "number",
    "code": "invalid_type",
    "path": [
      "iterations"
    ],
    "message": "Invalid input: expected number, received undefined"
  }
]"#;
        assert_eq!(result, err(expected));
    }

    #[test]
    fn 타입이_다르면_받은_타입_이름을_알려준다() {
        let found = issues(json!({
            "input": null, "algorithm": 5, "iterations": "5",
            "headers": [], "transforms": {}, "flag": "x"
        }));
        let messages: Vec<&str> = found
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|issue| issue["message"].as_str())
            .collect();
        assert_eq!(
            messages,
            [
                "Invalid input: expected string, received null",
                "Invalid option: expected one of \"md5\"|\"sha1\"",
                "Invalid input: expected number, received string",
                "Invalid input: expected record, received array",
                "Invalid input: expected array, received object",
                "Invalid input: expected boolean, received string",
            ]
        );
    }

    #[test]
    fn 선택_인자에_null을_주면_거부한다() {
        let found = issues(with("flag", Value::Null));
        assert_eq!(
            found[0]["message"],
            "Invalid input: expected boolean, received null"
        );
    }

    #[test]
    fn 정수가_아니면_범위_검사_없이_int_오류만_낸다() {
        let found = issues(with("iterations", json!(-1.5)));
        assert_eq!(
            found,
            json!([{
                "expected": "int", "format": "safeint", "code": "invalid_type",
                "path": ["iterations"],
                "message": "Invalid input: expected int, received number"
            }])
        );
    }

    #[test]
    fn 정수의_하한과_상한을_검사한다() {
        assert_eq!(
            issues(with("iterations", json!(0))),
            json!([{
                "origin": "number", "code": "too_small", "minimum": 0, "inclusive": false,
                "path": ["iterations"], "message": "Too small: expected number to be >0"
            }])
        );
        assert_eq!(
            issues(with("iterations", json!(100001))),
            json!([{
                "origin": "number", "code": "too_big", "maximum": 100000, "inclusive": true,
                "path": ["iterations"], "message": "Too big: expected number to be <=100000"
            }])
        );
        assert!(validate(&def(), &with("iterations", json!(100000))).is_ok());
        // An integer written as a float is still an integer, as in JavaScript.
        assert!(validate(&def(), &with("iterations", json!(5.0))).is_ok());
    }

    #[test]
    fn 안전한_정수_범위를_넘으면_두_가지_오류를_함께_낸다() {
        let found = issues(with("iterations", json!(1_152_921_504_606_846_976_u64)));
        let messages: Vec<&str> = found
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|issue| issue["message"].as_str())
            .collect();
        assert_eq!(
            messages,
            [
                "Too big: expected int to be <=9007199254740991",
                "Too big: expected number to be <=100000",
            ]
        );
    }

    #[test]
    fn 레코드와_배열은_원소마다_경로를_붙여_검사한다() {
        let found = issues(json!({
            "input": "a", "algorithm": "md5", "iterations": 1,
            "headers": { "a": 1 }, "transforms": ["a", "c", 3]
        }));
        let paths: Vec<&Value> = found
            .as_array()
            .into_iter()
            .flatten()
            .map(|issue| &issue["path"])
            .collect();
        assert_eq!(
            paths,
            [
                &json!(["headers", "a"]),
                &json!(["transforms", 1]),
                &json!(["transforms", 2])
            ]
        );
        assert_eq!(
            found[0]["message"],
            "Invalid input: expected string, received number"
        );
        assert_eq!(
            found[2]["message"],
            "Invalid option: expected one of \"a\"|\"b\""
        );
    }

    #[test]
    fn 인자가_객체가_아니면_거부한다() {
        assert_eq!(
            issues(Value::Null)[0]["message"],
            "Invalid input: expected object, received undefined"
        );
        assert_eq!(
            issues(json!([]))[0]["message"],
            "Invalid input: expected object, received array"
        );
        assert_eq!(
            issues(json!("str")),
            json!([{
                "expected": "object", "code": "invalid_type", "path": [],
                "message": "Invalid input: expected object, received string"
            }])
        );
    }
}
