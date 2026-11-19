//! Argument validation for the tools of this crate: what the zod input schemas
//! check in the TypeScript before a handler runs.
//!
//! Like zod's default object, unknown keys are ignored and an optional key must
//! be absent rather than null.

use serde_json::{Map, Value};

pub(crate) struct Args<'a> {
    tool: &'static str,
    map: Option<&'a Map<String, Value>>,
}

impl<'a> Args<'a> {
    /// A missing arguments object (null) reads as an empty one.
    pub(crate) fn new(tool: &'static str, args: &'a Value) -> Result<Self, String> {
        let map = match args {
            Value::Object(map) => Some(map),
            Value::Null => None,
            _ => return Err(invalid(tool, "arguments", "expected an object")),
        };
        Ok(Self { tool, map })
    }

    fn get(&self, key: &str) -> Option<&'a Value> {
        self.map.and_then(|map| map.get(key))
    }

    fn fail<T>(&self, key: &str, problem: &str) -> Result<T, String> {
        Err(invalid(self.tool, key, problem))
    }

    fn required<T>(&self, key: &str, value: Option<T>) -> Result<T, String> {
        value.map_or_else(|| self.fail(key, "required"), Ok)
    }

    pub(crate) fn opt_str(&self, key: &str) -> Result<Option<&'a str>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => self.fail(key, "expected a string"),
        }
    }

    pub(crate) fn str(&self, key: &str) -> Result<&'a str, String> {
        let value = self.opt_str(key)?;
        self.required(key, value)
    }

    pub(crate) fn opt_bool(&self, key: &str) -> Result<Option<bool>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Bool(flag)) => Ok(Some(*flag)),
            Some(_) => self.fail(key, "expected a boolean"),
        }
    }

    /// `z.number()`: any JSON number.
    pub(crate) fn opt_number(&self, key: &str) -> Result<Option<f64>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(value) => match value.as_f64() {
                Some(number) => Ok(Some(number)),
                None => self.fail(key, "expected a number"),
            },
        }
    }

    /// `z.number().int()` within `min..=max`. `5.0` is an integer, as it is
    /// for JavaScript; without a `max` the limit is zod's, the largest integer
    /// a JavaScript number holds exactly.
    pub(crate) fn opt_int(
        &self,
        key: &str,
        min: u64,
        max: Option<u64>,
    ) -> Result<Option<usize>, String> {
        let Some(number) = self.opt_number(key)? else {
            return Ok(None);
        };
        if number.fract() != 0.0 || !number.is_finite() {
            return self.fail(key, "expected an integer");
        }
        if number < min as f64 {
            return self.fail(key, &format!("must be at least {min}"));
        }
        let max = max.unwrap_or(MAX_SAFE_INTEGER);
        if number > max as f64 {
            return self.fail(key, &format!("must be at most {max}"));
        }
        // Saturates where usize is narrower than the range just checked.
        Ok(Some(number as usize))
    }

    pub(crate) fn int(&self, key: &str, min: u64, max: Option<u64>) -> Result<usize, String> {
        let value = self.opt_int(key, min, max)?;
        self.required(key, value)
    }

    pub(crate) fn opt_enum(
        &self,
        key: &str,
        allowed: &[&'static str],
    ) -> Result<Option<&'static str>, String> {
        let Some(text) = self.opt_str(key)? else {
            return Ok(None);
        };
        match allowed.iter().find(|option| **option == text) {
            Some(option) => Ok(Some(option)),
            None => self.fail(key, &format!("expected one of {}", allowed.join(" | "))),
        }
    }

    pub(crate) fn enumeration(
        &self,
        key: &str,
        allowed: &[&'static str],
    ) -> Result<&'static str, String> {
        let value = self.opt_enum(key, allowed)?;
        self.required(key, value)
    }

    pub(crate) fn opt_str_array(&self, key: &str) -> Result<Option<Vec<&'a str>>, String> {
        let items = match self.get(key) {
            None => return Ok(None),
            Some(Value::Array(items)) => items,
            Some(_) => return self.fail(key, "expected an array of strings"),
        };
        let strings: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
        if strings.len() != items.len() {
            return self.fail(key, "expected an array of strings");
        }
        Ok(Some(strings))
    }
}

/// `Number.MAX_SAFE_INTEGER`.
pub(crate) const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn invalid(tool: &str, key: &str, problem: &str) -> String {
    format!("Input validation error: Invalid arguments for tool {tool}: {key}: {problem}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn 없는_선택_인자는_none이고_null은_거절한다() {
        let value = json!({ "host": null });
        let args = Args::new("t", &value).unwrap();
        assert_eq!(args.opt_str("missing"), Ok(None));
        assert_eq!(
            args.opt_str("host"),
            Err(
                "Input validation error: Invalid arguments for tool t: host: expected a string"
                    .to_owned()
            )
        );
    }

    #[test]
    fn 인자_객체가_없으면_빈_객체로_읽고_객체가_아니면_거절한다() {
        assert_eq!(Args::new("t", &Value::Null).unwrap().opt_str("a"), Ok(None));
        assert!(Args::new("t", &json!([1])).is_err());
        assert!(Args::new("t", &json!("x")).is_err());
    }

    #[test]
    fn 필수_인자가_없으면_거절한다() {
        let value = json!({});
        let args = Args::new("t", &value).unwrap();
        assert!(
            args.str("requestId")
                .unwrap_err()
                .ends_with("requestId: required")
        );
        assert!(args.int("byteOffset", 0, None).is_err());
        assert!(args.enumeration("action", &["start"]).is_err());
    }

    #[test]
    fn 정수는_범위와_정수_여부를_확인한다() {
        let value = json!({ "a": 5, "b": 5.0, "c": 5.5, "d": 0, "e": 201, "f": "5", "g": -1, "h": 1e300, "i": MAX_SAFE_INTEGER, "j": MAX_SAFE_INTEGER + 1 });
        let args = Args::new("t", &value).unwrap();
        assert_eq!(args.opt_int("a", 1, Some(200)), Ok(Some(5)));
        assert_eq!(args.opt_int("b", 1, Some(200)), Ok(Some(5)));
        assert!(args.opt_int("c", 1, Some(200)).is_err());
        assert!(args.opt_int("d", 1, Some(200)).is_err());
        assert_eq!(args.opt_int("d", 0, Some(200)), Ok(Some(0)));
        assert!(args.opt_int("e", 1, Some(200)).is_err());
        assert!(args.opt_int("f", 1, None).is_err());
        assert!(args.opt_int("g", 0, None).is_err());
        // Without a maximum, zod's `int()` stops at Number.MAX_SAFE_INTEGER.
        assert!(args.opt_int("h", 0, None).is_err());
        assert_eq!(
            args.opt_int("i", 0, None),
            Ok(Some(MAX_SAFE_INTEGER as usize))
        );
        assert!(args.opt_int("j", 0, None).is_err());
    }

    #[test]
    fn 열거형과_문자열_배열과_불리언을_확인한다() {
        let value = json!({ "action": "stop", "bad": "pause", "types": ["click", "submit"], "mixed": ["a", 1], "flag": true, "n": 1.5 });
        let args = Args::new("t", &value).unwrap();
        assert_eq!(args.enumeration("action", &["start", "stop"]), Ok("stop"));
        assert!(
            args.opt_enum("bad", &["start", "stop"])
                .unwrap_err()
                .ends_with("bad: expected one of start | stop")
        );
        assert_eq!(
            args.opt_str_array("types"),
            Ok(Some(vec!["click", "submit"]))
        );
        assert!(args.opt_str_array("mixed").is_err());
        assert!(args.opt_str_array("flag").is_err());
        assert_eq!(args.opt_bool("flag"), Ok(Some(true)));
        assert!(args.opt_bool("action").is_err());
        assert_eq!(args.opt_number("n"), Ok(Some(1.5)));
        assert!(args.opt_number("flag").is_err());
    }
}
