//! Argument validation. The TypeScript declared each tool's arguments with zod
//! and the MCP SDK refused a bad call before the handler ran; here the same
//! checks are read off the tool's JSON Schema, and the refusal is the text the
//! SDK returned (zod's issue list), so an agent reads the same correction.

use rever_tools_api::tool::ToolDef;
use serde_json::{Map, Value, json};

use crate::js::json_stringify;

/// `Number.MAX_SAFE_INTEGER`, the bound zod's `.int()` enforces.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Checks `args` against the tool's input schema. `Err` is the refusal text.
pub(crate) fn validate(def: &ToolDef, args: &Value) -> Result<(), String> {
    let schema = &def.input_schema;
    let takes_arguments = schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| !properties.is_empty());
    let mut issues = Vec::new();
    match args {
        Value::Object(map) => check_properties(schema, map, &[], &mut issues),
        // A call without arguments reaches the tool as null.
        Value::Null if !takes_arguments => {}
        Value::Null => issues.push(invalid_type("object", None, &[])),
        other => issues.push(invalid_type("object", Some(other), &[])),
    }
    if issues.is_empty() {
        return Ok(());
    }
    Err(format!(
        "MCP error -32602: Input validation error: Invalid arguments for tool {}: {}",
        def.name,
        json_stringify(&Value::Array(issues), true)
    ))
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

fn invalid_type(expected: &str, value: Option<&Value>, path: &[Value]) -> Value {
    json!({
        "expected": expected,
        "code": "invalid_type",
        "path": path,
        "message": format!("Invalid input: expected {expected}, received {}", received(value)),
    })
}

fn child(path: &[Value], key: Value) -> Vec<Value> {
    let mut path = path.to_vec();
    path.push(key);
    path
}

/// `z.object`: a missing optional key is fine, an unknown key is ignored.
fn check_properties(
    schema: &Value,
    map: &Map<String, Value>,
    path: &[Value],
    issues: &mut Vec<Value>,
) {
    let required = schema.get("required").and_then(Value::as_array);
    let properties = schema.get("properties").and_then(Value::as_object);
    for (name, property) in properties.into_iter().flatten() {
        let value = map.get(name);
        let is_required = required.is_some_and(|names| names.iter().any(|n| n == name));
        if value.is_some() || is_required {
            check(property, value, &child(path, json!(name)), issues);
        }
    }
}

fn check(schema: &Value, value: Option<&Value>, path: &[Value], issues: &mut Vec<Value>) {
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !value.is_some_and(|value| values.contains(value)) {
            let options: Vec<String> = values.iter().map(Value::to_string).collect();
            issues.push(json!({
                "code": "invalid_value",
                "values": values,
                "path": path,
                "message": format!("Invalid option: expected one of {}", options.join("|")),
            }));
        }
        return;
    }
    let Some(kind) = schema.get("type").and_then(Value::as_str) else {
        return; // z.unknown()
    };
    match (kind, value) {
        ("string", Some(Value::String(_)))
        | ("boolean", Some(Value::Bool(_)))
        | ("number", Some(Value::Number(_))) => {}
        ("integer", Some(Value::Number(number))) => {
            check_integer(schema, number.as_f64().unwrap_or(0.0), path, issues);
        }
        ("integer", _) => issues.push(invalid_type("number", value, path)),
        ("array", Some(Value::Array(items))) => {
            if let Some(item) = schema.get("items") {
                for (index, entry) in items.iter().enumerate() {
                    check(item, Some(entry), &child(path, json!(index)), issues);
                }
            }
            check_length(schema, items.len(), path, issues);
        }
        ("object", Some(Value::Object(map))) => {
            if schema.get("properties").is_some() {
                check_properties(schema, map, path, issues);
            } else if let Some(item) = schema.get("additionalProperties") {
                for (key, entry) in map {
                    check(item, Some(entry), &child(path, json!(key)), issues);
                }
            }
        }
        // An object without declared properties is a `z.record`.
        ("object", _) if schema.get("properties").is_none() => {
            issues.push(invalid_type("record", value, path));
        }
        _ => issues.push(invalid_type(kind, value, path)),
    }
}

/// `z.number().int()` with the bounds the schema carries.
fn check_integer(schema: &Value, number: f64, path: &[Value], issues: &mut Vec<Value>) {
    if number.fract() != 0.0 {
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
    let safe = MAX_SAFE_INTEGER as f64;
    if number.abs() > safe {
        let (code, key, bound, word, sign) = if number > 0.0 {
            ("too_big", "maximum", MAX_SAFE_INTEGER, "Too big", "<=")
        } else {
            ("too_small", "minimum", -MAX_SAFE_INTEGER, "Too small", ">=")
        };
        issues.push(json!({
            "code": code,
            key: bound,
            "note": "Integers must be within the safe integer range.",
            "origin": "int",
            "inclusive": true,
            "path": path,
            "message": format!("{word}: expected int to be {sign}{bound}"),
        }));
    }
    // A bound equal to the safe range is the one `.int()` put in the schema
    // itself, already reported above.
    let bound = |key: &str| {
        let bound = schema.get(key)?;
        (bound.as_f64()?.abs() != safe).then_some(bound)
    };
    if let Some(minimum) = bound("minimum").filter(|m| m.as_f64().is_some_and(|m| number < m)) {
        issues.push(json!({
            "origin": "number",
            "code": "too_small",
            "minimum": minimum,
            "inclusive": true,
            "path": path,
            "message": format!("Too small: expected number to be >={minimum}"),
        }));
    }
    if let Some(maximum) = bound("maximum").filter(|m| m.as_f64().is_some_and(|m| number > m)) {
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

/// `.min()` and `.max()` of an array, reported after its items.
fn check_length(schema: &Value, length: usize, path: &[Value], issues: &mut Vec<Value>) {
    let bound = |key: &str| schema.get(key).and_then(Value::as_u64);
    let length = length as u64;
    if let Some(minimum) = bound("minItems").filter(|minimum| length < *minimum) {
        issues.push(json!({
            "origin": "array",
            "code": "too_small",
            "minimum": minimum,
            "inclusive": true,
            "path": path,
            "message": format!("Too small: expected array to have >={minimum} items"),
        }));
    }
    if let Some(maximum) = bound("maxItems").filter(|maximum| length > *maximum) {
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

/// A string argument, once `validate` has passed.
pub(crate) fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use rever_tools_api::tool::ToolRegistry;

    use super::*;

    #[test]
    fn 잘못된_인자는_타입스크립트와_같은_문구로_거절한다() {
        // The fixture is what the MCP SDK answered (through zod) for each call,
        // dumped with bun against macros.ts and cli.ts.
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("../fixtures/ts-validation.json")).unwrap();
        let mut registry = ToolRegistry::default();
        crate::register(&mut registry);
        assert!(cases.len() > 25);
        for case in &cases {
            let name = case["name"].as_str().unwrap();
            let def = registry.find(name).unwrap().def();
            // A call without arguments is recorded without an `args` key.
            let args = case.get("args").cloned().unwrap_or(Value::Null);
            let refusal = validate(&def, &args).unwrap_err();
            assert_eq!(Value::from(refusal), case["text"], "{name} {args}");
        }
    }

    #[test]
    fn 모르는_키는_무시하고_인자_없는_도구는_인자가_없어도_통과한다() {
        let mut registry = ToolRegistry::default();
        crate::register(&mut registry);
        let def = |name: &str| registry.find(name).unwrap().def();
        assert_eq!(validate(&def("list_macros"), &Value::Null), Ok(()));
        assert_eq!(
            validate(&def("list_macros"), &json!({ "extra": 1 })),
            Ok(())
        );
        assert_eq!(
            validate(&def("get_macro"), &json!({ "id": "m", "extra": 1 })),
            Ok(())
        );
        assert!(validate(&def("get_macro"), &json!("m")).is_err());
        let step =
            json!({ "tool": "t", "input": { "any": [1, null] }, "delay": 1.5, "more": true });
        let args = json!({ "name": "n", "steps": [step] });
        assert_eq!(validate(&def("create_macro"), &args), Ok(()));
    }
}
