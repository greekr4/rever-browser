//! The argument checks the TypeScript MCP SDK ran (through zod) before a tool's
//! handler: here validating is the tool's job, and the agent must read the same
//! refusal for the same bad arguments.

use rever_tools_api::utils::{ToolResult, err};
use serde_json::{Map, Value, json};

/// `Number.MAX_SAFE_INTEGER`, the bound zod's `.int()` enforces.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// zod's name for the type of the value it was handed.
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

fn invalid_type(expected: &str, value: Option<&Value>, path: &[&str]) -> Value {
    json!({
        "expected": expected,
        "code": "invalid_type",
        "path": path,
        "message": format!("Invalid input: expected {expected}, received {}", received(value)),
    })
}

/// The arguments object. A call without `arguments` reaches the tool as `null`
/// and is refused like the `undefined` it was in the TypeScript.
pub(crate) fn object(args: &Value) -> Result<&Map<String, Value>, Value> {
    match args {
        Value::Object(map) => Ok(map),
        Value::Null => Err(invalid_type("object", None, &[])),
        other => Err(invalid_type("object", Some(other), &[])),
    }
}

/// `z.string()`.
pub(crate) fn string<'a>(
    args: &'a Map<String, Value>,
    key: &str,
    issues: &mut Vec<Value>,
) -> Option<&'a str> {
    let value = args.get(key);
    let text = value.and_then(Value::as_str);
    if text.is_none() {
        issues.push(invalid_type("string", value, &[key]));
    }
    text
}

/// `z.number().int()`. Returns the number when later bound checks still run:
/// zod stops at a fractional number but keeps checking an integer that is
/// outside the safe range.
pub(crate) fn int(value: Option<&Value>, key: &str, issues: &mut Vec<Value>) -> Option<f64> {
    let Some(number) = value.and_then(Value::as_f64) else {
        issues.push(invalid_type("number", value, &[key]));
        return None;
    };
    if number.fract() != 0.0 {
        issues.push(json!({
            "expected": "int",
            "format": "safeint",
            "code": "invalid_type",
            "path": [key],
            "message": "Invalid input: expected int, received number",
        }));
        return None;
    }
    if number > MAX_SAFE_INTEGER {
        issues.push(json!({
            "code": "too_big",
            "maximum": 9_007_199_254_740_991_i64,
            "note": "Integers must be within the safe integer range.",
            "origin": "int",
            "inclusive": true,
            "path": [key],
            "message": "Too big: expected int to be <=9007199254740991",
        }));
    } else if number < -MAX_SAFE_INTEGER {
        issues.push(json!({
            "code": "too_small",
            "minimum": -9_007_199_254_740_991_i64,
            "note": "Integers must be within the safe integer range.",
            "origin": "int",
            "inclusive": true,
            "path": [key],
            "message": "Too small: expected int to be >=-9007199254740991",
        }));
    }
    Some(number)
}

/// `.nonnegative()` (inclusive) and `.positive()` (exclusive) both bound at 0.
pub(crate) fn at_least_zero(number: f64, inclusive: bool, key: &str, issues: &mut Vec<Value>) {
    if number > 0.0 || (inclusive && number == 0.0) {
        return;
    }
    let sign = if inclusive { ">=" } else { ">" };
    issues.push(json!({
        "origin": "number",
        "code": "too_small",
        "minimum": 0,
        "inclusive": inclusive,
        "path": [key],
        "message": format!("Too small: expected number to be {sign}0"),
    }));
}

/// `.max(maximum)`.
pub(crate) fn at_most(number: f64, maximum: u32, key: &str, issues: &mut Vec<Value>) {
    if number <= f64::from(maximum) {
        return;
    }
    issues.push(json!({
        "origin": "number",
        "code": "too_big",
        "maximum": maximum,
        "inclusive": true,
        "path": [key],
        "message": format!("Too big: expected number to be <={maximum}"),
    }));
}

/// The result the SDK returned for arguments its schema refused: the issue
/// list, printed as `JSON.stringify(issues, null, 2)`.
pub(crate) fn invalid_arguments(tool: &str, issues: &[Value]) -> ToolResult {
    let detail = serde_json::to_string_pretty(issues).unwrap_or_default();
    err(format!(
        "MCP error -32602: Input validation error: Invalid arguments for tool {tool}: {detail}"
    ))
}
