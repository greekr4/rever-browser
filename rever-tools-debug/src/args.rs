//! Argument validation as the MCP SDK does it before a handler runs: the arguments
//! are checked against the tool's schema and every problem is reported at once, in
//! the words zod uses, because that text is what the agent reads and corrects from.
//!
//! The check walks the tool's own JSON Schema, so what `tools/list` promises and
//! what a call enforces cannot drift apart. It knows the constructs these tools
//! use: string, enum, integer, array, object, strict object and record.

use rever_tools_api::tool::ToolDef;
use serde_json::{Map, Value, json};

// Number.MAX_SAFE_INTEGER: zod's `.int()` only accepts integers a double holds exactly.
pub(crate) const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// `Err` carries the text of the `isError` result the SDK answers with.
pub fn validate(def: &ToolDef, args: &Value) -> Result<(), String> {
    let schema = &def.input_schema;
    // The SDK does not look at the arguments of a tool registered without a schema.
    if schema
        .get("properties")
        .and_then(Value::as_object)
        .is_none_or(Map::is_empty)
    {
        return Ok(());
    }
    // Absent arguments reach a tool as null.
    let args = Some(args).filter(|args| !args.is_null());
    let mut issues = Vec::new();
    check(schema, args, &mut Vec::new(), &mut issues);
    if issues.is_empty() {
        return Ok(());
    }
    Err(format!(
        "MCP error -32602: Input validation error: Invalid arguments for tool {}: {}",
        def.name,
        crate::pretty(&issues)
    ))
}

/// An integer argument that passed [`validate`]; JSON may spell it `3` or `3.0`.
pub fn int(args: &Value, key: &str) -> Option<i64> {
    let value = args.get(key)?;
    value
        .as_i64()
        // Validation bounds it to the safe-integer range, so the cast is exact.
        .or_else(|| value.as_f64().map(|number| number as i64))
}

pub fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)?.as_str()
}

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
        ("integer", Some(Value::Number(number))) => {
            check_integer(schema, number.as_f64().unwrap_or(0.0), path, issues);
        }
        ("integer", value) => issues.push(invalid_type("number", value, path)),
        ("array", Some(Value::Array(items))) => {
            let item_schema = schema.get("items").unwrap_or(&Value::Null);
            for (index, item) in items.iter().enumerate() {
                path.push(json!(index));
                check(item_schema, Some(item), path, issues);
                path.pop();
            }
        }
        ("array", value) => issues.push(invalid_type("array", value, path)),
        ("object", Some(Value::Object(fields))) => check_object(schema, fields, path, issues),
        ("object", value) => {
            let expected = match schema.get("properties") {
                Some(_) => "object",
                None => "record",
            };
            issues.push(invalid_type(expected, value, path));
        }
        _ => {}
    }
}

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
    // The schema always lists a minimum; only one above the safe-integer floor is
    // a check of its own (`.nonnegative()`).
    let minimum = schema.get("minimum").and_then(Value::as_i64);
    if let Some(minimum) = minimum.filter(|&min| min > -MAX_SAFE_INTEGER && number < min as f64) {
        issues.push(json!({
            "origin": "number",
            "code": "too_small",
            "minimum": minimum,
            "inclusive": true,
            "path": path,
            "message": format!("Too small: expected number to be >={minimum}"),
        }));
    }
}

fn check_object(
    schema: &Value,
    fields: &Map<String, Value>,
    path: &mut Vec<Value>,
    issues: &mut Vec<Value>,
) {
    let additional = schema.get("additionalProperties");
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        // A record: every value follows the one value schema.
        if let Some(value_schema) = additional.filter(|schema| schema.is_object()) {
            for (key, value) in fields {
                path.push(json!(key));
                check(value_schema, Some(value), path, issues);
                path.pop();
            }
        }
        return;
    };
    let required = schema.get("required").and_then(Value::as_array);
    for (key, property) in properties {
        let field = fields.get(key);
        let is_required =
            required.is_some_and(|names| names.iter().any(|name| name.as_str() == Some(key)));
        // Optional means absent; null is a value of the wrong type.
        if field.is_none() && !is_required {
            continue;
        }
        path.push(json!(key));
        check(property, field, path, issues);
        path.pop();
    }
    // Unknown keys are dropped silently unless the object is strict.
    if additional == Some(&Value::Bool(false)) {
        let unknown: Vec<&String> = fields
            .keys()
            .filter(|key| !properties.contains_key(*key))
            .collect();
        if !unknown.is_empty() {
            let quoted: Vec<String> = unknown.iter().map(|key| json!(key).to_string()).collect();
            let label = if unknown.len() == 1 { "key" } else { "keys" };
            issues.push(json!({
                "code": "unrecognized_keys",
                "keys": unknown,
                "path": path,
                "message": format!("Unrecognized {label}: {}", quoted.join(", ")),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use rever_tools_api::tool::{Tool, ToolRegistry};

    use super::*;

    fn def(name: &str) -> ToolDef {
        let mut registry = ToolRegistry::default();
        crate::register(&mut registry);
        registry.find(name).expect("tool is registered").def()
    }

    /// The issues of a rejected call, without the SDK's prefix.
    fn issues(name: &str, args: Value) -> Value {
        let error = validate(&def(name), &args).expect_err("arguments were accepted");
        let prefix = format!(
            "MCP error -32602: Input validation error: Invalid arguments for tool {name}: "
        );
        let listed = error.strip_prefix(&prefix).expect("SDK prefix");
        serde_json::from_str(listed).expect("issues are JSON")
    }

    // Every expected text below is what the TypeScript tool answered to the same
    // arguments, called through the MCP SDK with bun.

    #[test]
    fn 필수_인자가_없으면_모든_문제를_한번에_zod_문구로_알린다() {
        let error = validate(&def("bp_add"), &json!({})).unwrap_err();
        assert_eq!(
            error,
            "MCP error -32602: Input validation error: Invalid arguments for tool bp_add: [\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"urlGlob\"\n    ],\n    \"message\": \"Invalid input: expected string, received undefined\"\n  },\n  {\n    \"expected\": \"number\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"line\"\n    ],\n    \"message\": \"Invalid input: expected number, received undefined\"\n  }\n]"
        );
    }

    #[test]
    fn 인자_객체가_아예_없으면_객체가_필요하다고_알린다() {
        let error = validate(&def("bp_remove"), &Value::Null).unwrap_err();
        assert_eq!(
            error,
            "MCP error -32602: Input validation error: Invalid arguments for tool bp_remove: [\n  {\n    \"expected\": \"object\",\n    \"code\": \"invalid_type\",\n    \"path\": [],\n    \"message\": \"Invalid input: expected object, received undefined\"\n  }\n]"
        );
    }

    #[test]
    fn 스키마_없이_등록된_도구는_인자를_검사하지_않는다() {
        assert_eq!(validate(&def("bp_list"), &json!({ "junk": 1 })), Ok(()));
        assert_eq!(validate(&def("intercept_pending"), &Value::Null), Ok(()));
    }

    #[test]
    fn 모르는_키는_무시하고_올바른_인자는_통과시킨다() {
        let args = json!({ "urlGlob": "a", "line": 1, "extra": 1 });
        assert_eq!(validate(&def("bp_add"), &args), Ok(()));
        let args = json!({ "urlGlob": "a", "line": 0, "column": 3.0, "condition": "" });
        assert_eq!(validate(&def("bp_add"), &args), Ok(()));
    }

    #[test]
    fn 타입이_틀리면_받은_타입_이름을_알린다() {
        assert_eq!(
            issues(
                "bp_add",
                json!({ "urlGlob": 1, "line": true, "column": null })
            ),
            json!([
                { "expected": "string", "code": "invalid_type", "path": ["urlGlob"],
                  "message": "Invalid input: expected string, received number" },
                { "expected": "number", "code": "invalid_type", "path": ["line"],
                  "message": "Invalid input: expected number, received boolean" },
                { "expected": "number", "code": "invalid_type", "path": ["column"],
                  "message": "Invalid input: expected number, received null" }
            ])
        );
    }

    #[test]
    fn 정수가_아닌_수는_범위_검사_없이_int_문제_하나만_낸다() {
        assert_eq!(
            issues("bp_add", json!({ "urlGlob": "a", "line": -1.5 })),
            json!([
                { "expected": "int", "format": "safeint", "code": "invalid_type", "path": ["line"],
                  "message": "Invalid input: expected int, received number" }
            ])
        );
    }

    #[test]
    fn 음수_줄_번호는_too_small로_거부한다() {
        let error = validate(&def("bp_add"), &json!({ "urlGlob": "a", "line": -1 })).unwrap_err();
        assert!(error.ends_with(
            "[\n  {\n    \"origin\": \"number\",\n    \"code\": \"too_small\",\n    \"minimum\": 0,\n    \"inclusive\": true,\n    \"path\": [\n      \"line\"\n    ],\n    \"message\": \"Too small: expected number to be >=0\"\n  }\n]"
        ));
    }

    #[test]
    fn 안전한_정수_범위를_넘으면_too_big_또는_too_small을_낸다() {
        let too_big = json!([
            { "code": "too_big", "maximum": 9007199254740991i64,
              "note": "Integers must be within the safe integer range.",
              "origin": "int", "inclusive": true, "path": ["line"],
              "message": "Too big: expected int to be <=9007199254740991" }
        ]);
        let args = json!({ "urlGlob": "a", "line": 9007199254740992i64 });
        assert_eq!(issues("bp_add", args), too_big);
        assert_eq!(
            issues("bp_add", json!({ "urlGlob": "a", "line": 1e300 })),
            too_big
        );
        // The largest safe integer itself is fine.
        let args = json!({ "urlGlob": "a", "line": 9007199254740991i64 });
        assert_eq!(validate(&def("bp_add"), &args), Ok(()));

        // Below the floor on a non-negative field: both checks report.
        assert_eq!(
            issues(
                "bp_add",
                json!({ "urlGlob": "a", "line": -9007199254740992i64 })
            ),
            json!([
                { "code": "too_small", "minimum": -9007199254740991i64,
                  "note": "Integers must be within the safe integer range.",
                  "origin": "int", "inclusive": true, "path": ["line"],
                  "message": "Too small: expected int to be >=-9007199254740991" },
                { "origin": "number", "code": "too_small", "minimum": 0, "inclusive": true,
                  "path": ["line"], "message": "Too small: expected number to be >=0" }
            ])
        );
        // A plain `.int()` has no minimum of its own: a negative status passes.
        let args = json!({ "requestId": "r", "responseCode": -1 });
        assert_eq!(validate(&def("intercept_fulfill"), &args), Ok(()));
    }

    #[test]
    fn 열거형은_없거나_타입이_틀려도_허용값_목록을_알린다() {
        let mode = json!({
            "code": "invalid_value", "values": ["log", "block", "modify"], "path": ["mode"],
            "message": "Invalid option: expected one of \"log\"|\"block\"|\"modify\""
        });
        let stage = json!({
            "code": "invalid_value", "values": ["Request", "Response"], "path": ["stage"],
            "message": "Invalid option: expected one of \"Request\"|\"Response\""
        });
        assert_eq!(
            issues("intercept_add", json!({ "urlPattern": "a" })),
            json!([mode])
        );
        assert_eq!(
            issues(
                "intercept_add",
                json!({ "urlPattern": "a", "mode": 3, "stage": null })
            ),
            json!([mode, stage])
        );
        // Enum values are case-sensitive.
        assert_eq!(
            issues(
                "intercept_add",
                json!({ "urlPattern": "a", "mode": "log", "stage": "request" })
            ),
            json!([stage])
        );
    }

    #[test]
    fn 레코드는_값마다_검사하고_객체가_아니면_record를_기대한다고_알린다() {
        assert_eq!(
            issues(
                "intercept_add",
                json!({ "urlPattern": "a", "mode": "log", "modifyHeaders": { "a": 1, "b": "ok" } })
            ),
            json!([
                { "expected": "string", "code": "invalid_type", "path": ["modifyHeaders", "a"],
                  "message": "Invalid input: expected string, received number" }
            ])
        );
        assert_eq!(
            issues(
                "intercept_add",
                json!({ "urlPattern": "a", "mode": "log", "modifyHeaders": [] })
            ),
            json!([
                { "expected": "record", "code": "invalid_type", "path": ["modifyHeaders"],
                  "message": "Invalid input: expected record, received array" }
            ])
        );
    }

    #[test]
    fn 배열_항목은_인덱스_경로로_알리고_항목의_남는_키는_무시한다() {
        assert_eq!(
            issues(
                "intercept_continue",
                json!({ "requestId": "r", "headers": [{ "name": "a" }, 3] })
            ),
            json!([
                { "expected": "string", "code": "invalid_type", "path": ["headers", 0, "value"],
                  "message": "Invalid input: expected string, received undefined" },
                { "expected": "object", "code": "invalid_type", "path": ["headers", 1],
                  "message": "Invalid input: expected object, received number" }
            ])
        );
        assert_eq!(
            issues(
                "intercept_continue",
                json!({ "requestId": "r", "headers": {} })
            ),
            json!([
                { "expected": "array", "code": "invalid_type", "path": ["headers"],
                  "message": "Invalid input: expected array, received object" }
            ])
        );
        let args = json!({
            "requestId": "r",
            "headers": [{ "name": "a", "value": "b", "extra": 1 }]
        });
        assert_eq!(validate(&def("intercept_continue"), &args), Ok(()));
    }

    #[test]
    fn 엄격한_객체는_모르는_키를_속성_문제_뒤에_알린다() {
        let error = validate(
            &def("replay_request"),
            &json!({ "requestId": "r", "overrides": { "nope": 1, "headers": { "a": 1 } } }),
        )
        .unwrap_err();
        assert_eq!(
            error,
            "MCP error -32602: Input validation error: Invalid arguments for tool replay_request: [\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"overrides\",\n      \"headers\",\n      \"a\"\n    ],\n    \"message\": \"Invalid input: expected string, received number\"\n  },\n  {\n    \"code\": \"unrecognized_keys\",\n    \"keys\": [\n      \"nope\"\n    ],\n    \"path\": [\n      \"overrides\"\n    ],\n    \"message\": \"Unrecognized key: \\\"nope\\\"\"\n  }\n]"
        );
        assert_eq!(
            issues(
                "replay_request",
                json!({ "requestId": "r", "overrides": { "a": 1, "b": 2 } })
            ),
            json!([
                { "code": "unrecognized_keys", "keys": ["a", "b"], "path": ["overrides"],
                  "message": "Unrecognized keys: \"a\", \"b\"" }
            ])
        );
    }

    #[tokio::test]
    async fn 모르는_키는_자바스크립트_객체의_키_순서로_알린다() {
        // Integer-like keys first, ascending: what the TypeScript answered (bun).
        let context = rever_tools_api::tool::ToolContext::for_tests();
        let args = json!({ "requestId": "r", "overrides": { "2": 1, "b": 1, "1": 1 } });
        let error = crate::test_support::call(&crate::intercept::ReplayRequest, &context, args)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            "MCP error -32602: Input validation error: Invalid arguments for tool replay_request: [\n  {\n    \"code\": \"unrecognized_keys\",\n    \"keys\": [\n      \"1\",\n      \"2\",\n      \"b\"\n    ],\n    \"path\": [\n      \"overrides\"\n    ],\n    \"message\": \"Unrecognized keys: \\\"1\\\", \\\"2\\\", \\\"b\\\"\"\n  }\n]"
        );
    }

    #[test]
    fn 배열_안의_null과_틀린_항목은_항목마다_알린다() {
        // What the TypeScript answered to the same arguments (bun).
        assert_eq!(
            issues(
                "intercept_continue",
                json!({ "requestId": "r1", "headers": [null, { "name": 1 }] })
            ),
            json!([
                { "expected": "object", "code": "invalid_type", "path": ["headers", 0],
                  "message": "Invalid input: expected object, received null" },
                { "expected": "string", "code": "invalid_type", "path": ["headers", 1, "name"],
                  "message": "Invalid input: expected string, received number" },
                { "expected": "string", "code": "invalid_type", "path": ["headers", 1, "value"],
                  "message": "Invalid input: expected string, received undefined" }
            ])
        );
    }

    #[test]
    fn 속성_문제는_입력_순서가_아니라_스키마_순서로_나온다() {
        assert_eq!(
            issues(
                "replay_request",
                json!({ "requestId": "r", "overrides": { "headers": [], "url": 1 } })
            ),
            json!([
                { "expected": "string", "code": "invalid_type", "path": ["overrides", "url"],
                  "message": "Invalid input: expected string, received number" },
                { "expected": "record", "code": "invalid_type", "path": ["overrides", "headers"],
                  "message": "Invalid input: expected record, received array" }
            ])
        );
    }

    #[test]
    fn 선택_객체에_null이나_배열을_주면_객체가_필요하다고_알린다() {
        for (given, name) in [
            (Value::Null, "null"),
            (json!([]), "array"),
            (json!("x"), "string"),
        ] {
            assert_eq!(
                issues(
                    "replay_request",
                    json!({ "requestId": "r", "overrides": given })
                ),
                json!([
                    { "expected": "object", "code": "invalid_type", "path": ["overrides"],
                      "message": format!("Invalid input: expected object, received {name}") }
                ])
            );
        }
    }

    #[test]
    fn 정수_인자는_소수점_표기로_와도_정수로_읽는다() {
        let args = json!({ "a": 3, "b": 4.0, "c": "x" });
        assert_eq!(int(&args, "a"), Some(3));
        assert_eq!(int(&args, "b"), Some(4));
        assert_eq!(int(&args, "c"), None);
        assert_eq!(int(&args, "missing"), None);
        assert_eq!(text(&args, "c"), Some("x"));
        assert_eq!(text(&args, "a"), None);
    }

    #[tokio::test]
    async fn 검사는_호출_경로에서도_핸들러보다_먼저_돈다() {
        // Through the tool itself: a bad call never reaches the state.
        let context = rever_tools_api::tool::ToolContext::for_tests();
        let result = crate::intercept::InterceptAdd
            .call(&context, json!({ "urlPattern": "a" }))
            .await;
        assert!(result.is_error);
        assert!(
            context
                .state::<crate::intercept::InterceptState>()
                .rules()
                .is_empty()
        );
    }
}
