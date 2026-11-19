//! Argument validation and input schemas: what zod and the MCP SDK did for the
//! TypeScript tools. A failed check is the tool's error result; the wording is
//! ours, the accepted values are zod's.

use serde_json::{Map, Value, json};

/// `Number.MAX_SAFE_INTEGER`: the bound zod's `.int()` puts on every integer.
pub(crate) const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

fn invalid(key: &str, expected: &str) -> String {
    format!("invalid arguments: `{key}` must be {expected}")
}

pub(crate) struct Args<'a>(Option<&'a Map<String, Value>>);

impl<'a> Args<'a> {
    pub(crate) fn new(value: &'a Value) -> Result<Self, String> {
        match value {
            // A call without arguments reaches a tool as null.
            Value::Null => Ok(Self(None)),
            Value::Object(map) => Ok(Self(Some(map))),
            _ => Err("invalid arguments: expected an object".to_owned()),
        }
    }

    // Unknown keys are ignored, as zod strips them. An explicit null is not
    // "absent": zod's `.optional()` rejects it, so each getter does too.
    fn get(&self, key: &str) -> Option<&'a Value> {
        self.0?.get(key)
    }

    pub(crate) fn str(&self, key: &str) -> Result<Option<&'a str>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(invalid(key, "a string")),
        }
    }

    pub(crate) fn required_str(&self, key: &str) -> Result<&'a str, String> {
        self.str(key)?
            .ok_or_else(|| invalid(key, "a string (required)"))
    }

    pub(crate) fn bool(&self, key: &str) -> Result<Option<bool>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Bool(flag)) => Ok(Some(*flag)),
            Some(_) => Err(invalid(key, "a boolean")),
        }
    }

    pub(crate) fn number(&self, key: &str) -> Result<Option<f64>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(value) => value
                .as_f64()
                .map(Some)
                .ok_or_else(|| invalid(key, "a number")),
        }
    }

    /// `z.number().int()` within `min..=max`. `5.0` is an integer, as in JavaScript.
    pub(crate) fn int(&self, key: &str, min: i64, max: i64) -> Result<Option<i64>, String> {
        let expected = || invalid(key, &format!("an integer from {min} to {max}"));
        let Some(number) = self.get(key) else {
            return Ok(None);
        };
        let number = number.as_f64().ok_or_else(expected)?;
        if number.fract() != 0.0 || number < min as f64 || number > max as f64 {
            return Err(expected());
        }
        Ok(Some(number as i64))
    }

    /// `z.number().int()` with no further bounds.
    pub(crate) fn any_int(&self, key: &str) -> Result<Option<i64>, String> {
        self.int(key, -MAX_SAFE_INTEGER, MAX_SAFE_INTEGER)
    }

    /// The `limit` argument: `z.number().int().positive().max(max)`.
    pub(crate) fn limit(&self, max: usize) -> Result<Option<usize>, String> {
        let max = i64::try_from(max).unwrap_or(MAX_SAFE_INTEGER);
        Ok(self
            .int("limit", 1, max)?
            .and_then(|limit| usize::try_from(limit).ok()))
    }

    /// An epoch-millisecond lower bound. Timestamps are whole milliseconds, so
    /// `t >= since` for a fractional or negative `since` is `t >= ceil(max(since, 0))`.
    pub(crate) fn since(&self) -> Result<Option<u64>, String> {
        Ok(self
            .number("since")?
            .map(|since| since.max(0.0).ceil() as u64))
    }
}

// ── Input schemas, in the shape the MCP SDK derives from a zod raw shape ─────

pub(crate) fn schema(properties: Value, required: &[&str]) -> Value {
    let mut schema = Map::new();
    schema.insert(
        "$schema".to_owned(),
        json!("http://json-schema.org/draft-07/schema#"),
    );
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), properties);
    if !required.is_empty() {
        schema.insert("required".to_owned(), json!(required));
    }
    Value::Object(schema)
}

/// What the SDK lists for a tool registered without an input schema.
pub(crate) fn no_input() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// A `string`, `number` or `boolean` property.
pub(crate) fn prop(kind: &str, description: &str) -> Value {
    json!({ "description": description, "type": kind })
}

/// `z.number().int()`.
pub(crate) fn int_prop(description: &str) -> Value {
    json!({
        "description": description,
        "type": "integer",
        "minimum": -MAX_SAFE_INTEGER,
        "maximum": MAX_SAFE_INTEGER
    })
}

/// `z.number().int().positive().max(max)`.
pub(crate) fn limit_prop(description: &str, max: usize) -> Value {
    json!({
        "description": description,
        "type": "integer",
        "exclusiveMinimum": 0,
        "maximum": max
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 인자가_없으면_빈_객체로_보고_객체가_아니면_거절한다() {
        let null = Value::Null;
        let args = Args::new(&null).unwrap();
        assert_eq!(args.str("host"), Ok(None));
        assert_eq!(args.limit(200), Ok(None));
        assert!(args.required_str("requestId").is_err());

        assert!(Args::new(&json!([1])).is_err());
        assert!(Args::new(&json!("x")).is_err());
    }

    #[test]
    fn 타입이_다른_값과_null은_거절한다() {
        let value = json!({ "host": 1, "full": "yes", "since": "1", "tab": null, "x": null });
        let args = Args::new(&value).unwrap();
        assert_eq!(
            args.str("host"),
            Err("invalid arguments: `host` must be a string".to_owned())
        );
        assert!(args.bool("full").is_err());
        assert!(args.number("since").is_err());
        assert!(args.any_int("tab").is_err());
        assert!(args.str("x").is_err());
        // Keys the tool does not read are ignored.
        assert_eq!(args.str("unknown"), Ok(None));
    }

    #[test]
    fn limit은_1부터_상한까지의_정수만_받는다() {
        let limit = |value: Value| {
            let value = json!({ "limit": value });
            Args::new(&value).and_then(|args| args.limit(200))
        };
        assert_eq!(limit(json!(1)), Ok(Some(1)));
        assert_eq!(limit(json!(200)), Ok(Some(200)));
        // JSON 5.0 is the integer 5 in JavaScript.
        assert_eq!(limit(json!(5.0)), Ok(Some(5)));
        assert_eq!(
            limit(json!(0)),
            Err("invalid arguments: `limit` must be an integer from 1 to 200".to_owned())
        );
        assert!(limit(json!(201)).is_err());
        assert!(limit(json!(-3)).is_err());
        assert!(limit(json!(1.5)).is_err());
        assert!(limit(json!("10")).is_err());
    }

    #[test]
    fn 정수는_안전한_정수_범위까지만_받는다() {
        let int = |value: Value| {
            let value = json!({ "tab": value });
            Args::new(&value).and_then(|args| args.any_int("tab"))
        };
        assert_eq!(int(json!(-1)), Ok(Some(-1)));
        assert_eq!(int(json!(MAX_SAFE_INTEGER)), Ok(Some(MAX_SAFE_INTEGER)));
        assert!(int(json!(1e300)).is_err());
        assert!(int(json!(0.5)).is_err());
    }

    #[test]
    fn since는_올림하고_음수는_0으로_본다() {
        let since = |value: Value| {
            let value = json!({ "since": value });
            Args::new(&value).and_then(|args| args.since())
        };
        assert_eq!(since(json!(1000)), Ok(Some(1000)));
        assert_eq!(since(json!(1000.2)), Ok(Some(1001)));
        assert_eq!(since(json!(-5)), Ok(Some(0)));
        assert!(since(json!(true)).is_err());
    }

    #[test]
    fn 스키마는_필수_인자가_있을_때만_required를_넣는다() {
        let optional = schema(json!({ "host": prop("string", "h") }), &[]);
        assert_eq!(
            optional,
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": { "host": { "description": "h", "type": "string" } }
            })
        );
        let required = schema(json!({}), &["requestId"]);
        assert_eq!(required.get("required"), Some(&json!(["requestId"])));
        assert_eq!(
            limit_prop("Max items", 200),
            json!({ "description": "Max items", "type": "integer", "exclusiveMinimum": 0, "maximum": 200 })
        );
    }
}
