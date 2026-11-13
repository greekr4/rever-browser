//! Argument validation. The TypeScript declared each tool's arguments with zod
//! and the MCP SDK rejected a bad call before the handler ran; here the same
//! checks are read off the tool's JSON Schema, and the error text is the one
//! the SDK returned (zod's issue list), so an agent sees the same correction.

use rever_tools_api::tool::ToolDef;
use rever_tools_api::utils::{ToolResult, err};
use serde_json::{Value, json};

/// Number.MAX_SAFE_INTEGER, the bound zod's `.int()` enforces.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

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
        serde_json::to_string_pretty(&issues).unwrap_or_default()
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
                let options: Vec<String> = allowed.map(Value::to_string).collect();
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
            check_integer(n.as_f64().unwrap_or(0.0), path, issues);
        }
        ("integer", _) => issues.push(invalid_type("number", value, path)),
        ("array", Some(Value::Array(items))) => {
            let minimum = schema.get("minItems").and_then(Value::as_u64).unwrap_or(0);
            if (items.len() as u64) < minimum {
                issues.push(json!({
                    "origin": "array",
                    "code": "too_small",
                    "minimum": minimum,
                    "inclusive": true,
                    "path": path,
                    "message": format!("Too small: expected array to have >={minimum} items"),
                }));
            }
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

/// `z.number().int()`: the only bounds are the safe-integer ones.
fn check_integer(n: f64, path: Vec<Value>, issues: &mut Vec<Value>) {
    if n.fract() != 0.0 {
        issues.push(json!({
            "expected": "int",
            "format": "safeint",
            "code": "invalid_type",
            "path": path,
            "message": "Invalid input: expected int, received number",
        }));
    } else if n > MAX_SAFE_INTEGER as f64 {
        issues.push(json!({
            "code": "too_big",
            "maximum": MAX_SAFE_INTEGER,
            "note": "Integers must be within the safe integer range.",
            "origin": "int",
            "inclusive": true,
            "path": path,
            "message": "Too big: expected int to be <=9007199254740991",
        }));
    } else if n < -(MAX_SAFE_INTEGER as f64) {
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
}

/// A string argument; `None` when absent (validation already rejected other types).
pub(crate) fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// A string-array argument; `None` when absent.
pub(crate) fn strings(args: &Value, key: &str) -> Option<Vec<String>> {
    let items = args.get(key)?.as_array()?;
    Some(
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    )
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
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "pages": { "type": "integer", "minimum": -MAX_SAFE_INTEGER, "maximum": MAX_SAFE_INTEGER },
                    "intensity": { "type": "string", "enum": ["readonly", "probe", "redteam"] },
                    "start": { "type": "boolean" },
                    "hosts": { "minItems": 1, "type": "array", "items": { "type": "string" } }
                },
                "required": ["url"]
            }),
        )
    }

    fn text_of(result: &ToolResult) -> &str {
        match result.content.first() {
            Some(Content::Text { text }) => text,
            None => "",
        }
    }

    /// The issue messages of a rejected call.
    fn messages(args: Value) -> Vec<String> {
        let Err(result) = validate(&def(), &args) else {
            return Vec::new();
        };
        assert!(result.is_error);
        let list = text_of(&result)
            .strip_prefix(
                "MCP error -32602: Input validation error: Invalid arguments for tool sample: ",
            )
            .unwrap_or("null");
        let issues: Value = serde_json::from_str(list).unwrap_or(Value::Null);
        let issues = issues.as_array().cloned().unwrap_or_default();
        issues
            .iter()
            .filter_map(|issue| issue["message"].as_str().map(str::to_owned))
            .collect()
    }

    #[test]
    fn 올바른_인자는_통과하고_모르는_키는_무시한다() {
        assert!(validate(&def(), &json!({ "url": "x" })).is_ok());
        assert!(validate(&def(), &json!({ "url": "x", "extra": 1 })).is_ok());
        let all = json!({
            "url": "", "pages": 5.0, "intensity": "probe", "start": false, "hosts": ["a"]
        });
        assert!(validate(&def(), &all).is_ok());
    }

    #[test]
    fn 잘못된_인자는_zod와_같은_문구로_거부한다() {
        // The text is what zod 4 printed for these arguments with this schema.
        let args = json!({ "url": "x", "pages": 1.5, "intensity": "bad", "start": "no" });
        let Err(result) = validate(&def(), &args) else {
            panic!("bad arguments must be rejected");
        };
        let expected = r#"MCP error -32602: Input validation error: Invalid arguments for tool sample: [
  {
    "expected": "int",
    "format": "safeint",
    "code": "invalid_type",
    "path": [
      "pages"
    ],
    "message": "Invalid input: expected int, received number"
  },
  {
    "code": "invalid_value",
    "values": [
      "readonly",
      "probe",
      "redteam"
    ],
    "path": [
      "intensity"
    ],
    "message": "Invalid option: expected one of \"readonly\"|\"probe\"|\"redteam\""
  },
  {
    "expected": "boolean",
    "code": "invalid_type",
    "path": [
      "start"
    ],
    "message": "Invalid input: expected boolean, received string"
  }
]"#;
        assert_eq!(result, err(expected));
    }

    #[test]
    fn 필수_인자가_없거나_타입이_다르면_받은_타입을_알려준다() {
        assert_eq!(
            messages(json!({ "pages": "3" })),
            [
                "Invalid input: expected string, received undefined",
                "Invalid input: expected number, received string",
            ]
        );
        // null does not count as absent for an optional argument.
        assert_eq!(
            messages(json!({ "url": "x", "hosts": null, "start": 1 })),
            [
                "Invalid input: expected boolean, received number",
                "Invalid input: expected array, received null",
            ]
        );
    }

    #[test]
    fn 안전한_정수_범위를_넘는_숫자를_거부한다() {
        let Err(result) = validate(&def(), &json!({ "url": "x", "pages": 1e300 })) else {
            panic!("an unsafe integer must be rejected");
        };
        let expected = r#"MCP error -32602: Input validation error: Invalid arguments for tool sample: [
  {
    "code": "too_big",
    "maximum": 9007199254740991,
    "note": "Integers must be within the safe integer range.",
    "origin": "int",
    "inclusive": true,
    "path": [
      "pages"
    ],
    "message": "Too big: expected int to be <=9007199254740991"
  }
]"#;
        assert_eq!(text_of(&result), expected);
        assert_eq!(
            messages(json!({ "url": "x", "pages": -1e300 })),
            ["Too small: expected int to be >=-9007199254740991"]
        );
    }

    #[test]
    fn 배열은_최소_길이와_원소_타입을_검사한다() {
        let Err(result) = validate(&def(), &json!({ "url": "x", "hosts": [] })) else {
            panic!("an empty array must be rejected");
        };
        let expected = r#"MCP error -32602: Input validation error: Invalid arguments for tool sample: [
  {
    "origin": "array",
    "code": "too_small",
    "minimum": 1,
    "inclusive": true,
    "path": [
      "hosts"
    ],
    "message": "Too small: expected array to have >=1 items"
  }
]"#;
        assert_eq!(text_of(&result), expected);

        let Err(result) = validate(&def(), &json!({ "url": "x", "hosts": [1, "a"] })) else {
            panic!("a non-string item must be rejected");
        };
        let expected = r#"MCP error -32602: Input validation error: Invalid arguments for tool sample: [
  {
    "expected": "string",
    "code": "invalid_type",
    "path": [
      "hosts",
      0
    ],
    "message": "Invalid input: expected string, received number"
  }
]"#;
        assert_eq!(text_of(&result), expected);
        assert_eq!(
            messages(json!({ "url": "x", "hosts": "a" })),
            ["Invalid input: expected array, received string"]
        );
    }

    #[test]
    fn 인자가_객체가_아니면_거부한다() {
        assert_eq!(
            messages(Value::Null),
            ["Invalid input: expected object, received undefined"]
        );
        assert_eq!(
            messages(json!([])),
            ["Invalid input: expected object, received array"]
        );
    }
}
