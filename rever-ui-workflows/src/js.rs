//! JavaScript semantics the ported code relies on: `trim`, truthiness, `String(value)`,
//! number formatting, `JSON.stringify(value, null, 2)` and UTF-16 slicing.

use rever_ui_state::format_json::try_pretty;
use serde_json::Value;

/// JavaScript's whitespace set (`String.prototype.trim` and the regex `\s`). Rust's
/// differs by two characters: U+FEFF is whitespace only in JS, U+0085 only in Rust.
pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `!!value` for a JSON value. `None` is `undefined`.
pub(crate) fn js_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// `Number.prototype.toString()`: the shortest digits that read back as the same
/// double, laid out the way ECMAScript does (exponent form outside 1e-7..1e21).
pub(crate) fn js_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    // Rust's `{:e}` prints the same shortest digits as "d.ddde<exp>".
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let (k, n) = (digits.len() as i32, exponent + 1);
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        let (whole, fraction) = digits.split_at(n as usize);
        format!("{whole}.{fraction}")
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let sign = if n > 0 { '+' } else { '-' };
        let (first, rest) = digits.split_at(1);
        let point = if rest.is_empty() { "" } else { "." };
        format!("{first}{point}{rest}e{sign}{}", (n - 1).abs())
    };
    if value < 0.0 {
        format!("-{body}")
    } else {
        body
    }
}

/// `String(value)` for a value `JSON.parse` produced. `None` is `undefined`.
pub(crate) fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(Value::Null) => "null".to_owned(),
        Some(Value::Bool(flag)) => flag.to_string(),
        // JSON.parse reads every number as a double, so large integers lose digits there too.
        Some(Value::Number(number)) => js_number(number.as_f64().unwrap_or(f64::NAN)),
        Some(Value::String(text)) => text.clone(),
        // Array.prototype.join: null and undefined items are empty.
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_owned(),
    }
}

/// `JSON.stringify(value, null, 2)`.
pub(crate) fn stringify_pretty(value: &Value) -> String {
    match value {
        // The pretty-printer of rever-ui-state takes text; a container always re-parses.
        Value::Array(_) | Value::Object(_) => try_pretty(&value.to_string()),
        Value::Number(number) => js_number(number.as_f64().unwrap_or(f64::NAN)),
        scalar => scalar.to_string(),
    }
}

/// A JSON number as JavaScript would write it: `100`, not `100.0`.
pub(crate) fn number_value(number: f64) -> Value {
    // Below 2^53 every whole double is an exact integer.
    if number.fract() == 0.0 && number.abs() < 9_007_199_254_740_992.0 {
        Value::from(number as i64)
    } else {
        // NaN and the infinities become null, as JSON.stringify writes them.
        serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number)
    }
}

/// `text.slice(0, units)` in UTF-16 code units. A surrogate pair split by the cut is
/// left out whole, since a Rust string cannot hold the lone half.
pub(crate) fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut seen = 0;
    for (at, c) in text.char_indices() {
        seen += c.len_utf16();
        if seen > units {
            return text.get(..at).unwrap_or(text);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 공백_다듬기는_자바스크립트_trim과_같다() {
        assert_eq!(js_trim("\u{feff} a b \u{3000}\n"), "a b");
        // U+0085는 Rust에서만 공백이다.
        assert_eq!(js_trim("\u{85}a\u{85}"), "\u{85}a\u{85}");
    }

    #[test]
    fn 참_거짓_판정은_자바스크립트와_같다() {
        for falsy in [json!(null), json!(false), json!(0), json!(0.0), json!("")] {
            assert!(!js_truthy(Some(&falsy)), "{falsy}");
        }
        assert!(!js_truthy(None));
        for truthy in [json!(true), json!(1), json!("0"), json!([]), json!({})] {
            assert!(js_truthy(Some(&truthy)), "{truthy}");
        }
    }

    #[test]
    fn 숫자는_자바스크립트_tostring처럼_쓴다() {
        assert_eq!(js_number(0.0), "0");
        assert_eq!(js_number(-0.0), "0");
        assert_eq!(js_number(100.0), "100");
        assert_eq!(js_number(-1.5), "-1.5");
        assert_eq!(js_number(0.1), "0.1");
        assert_eq!(js_number(0.000001), "0.000001");
        assert_eq!(js_number(0.0000001), "1e-7");
        assert_eq!(js_number(1.5e-7), "1.5e-7");
        assert_eq!(js_number(1e21), "1e+21");
        assert_eq!(js_number(1.5e21), "1.5e+21");
        assert_eq!(js_number(123456789012345680000.0), "123456789012345680000");
        assert_eq!(js_number(f64::NAN), "NaN");
    }

    #[test]
    fn 값을_문자열로_바꾸는_규칙은_string_함수와_같다() {
        assert_eq!(js_string(None), "undefined");
        assert_eq!(js_string(Some(&json!(null))), "null");
        assert_eq!(js_string(Some(&json!(true))), "true");
        assert_eq!(js_string(Some(&json!(3))), "3");
        assert_eq!(js_string(Some(&json!(2.50))), "2.5");
        assert_eq!(js_string(Some(&json!("a"))), "a");
        assert_eq!(js_string(Some(&json!([1, null, "x", [2, 3]]))), "1,,x,2,3");
        assert_eq!(js_string(Some(&json!({ "a": 1 }))), "[object Object]");
        // JSON.parse가 읽은 큰 정수는 double이다.
        assert_eq!(
            js_string(Some(&json!(12345678901234567890u64))),
            "12345678901234567000"
        );
    }

    #[test]
    fn 들여쓴_json은_두_칸이고_정수인_실수는_소수점_없이_쓴다() {
        assert_eq!(stringify_pretty(&json!({})), "{}");
        assert_eq!(
            stringify_pretty(&json!({ "url": "https://a.test", "n": 1.0 })),
            "{\n  \"url\": \"https://a.test\",\n  \"n\": 1\n}"
        );
        assert_eq!(
            stringify_pretty(&json!([1, [2]])),
            "[\n  1,\n  [\n    2\n  ]\n]"
        );
        assert_eq!(stringify_pretty(&json!("a\"b")), "\"a\\\"b\"");
        assert_eq!(stringify_pretty(&json!(2.0)), "2");
        assert_eq!(stringify_pretty(&json!(null)), "null");
    }

    // Expected values below were read from the TypeScript run under bun.
    #[test]
    fn 숫자의_경계값도_자바스크립트처럼_쓴다() {
        assert_eq!(js_number(5e-324), "5e-324");
        assert_eq!(js_number(1.7976931348623157e308), "1.7976931348623157e+308");
        assert_eq!(js_number(1e20), "100000000000000000000");
        assert_eq!(js_number(-1e-7), "-1e-7");
        assert_eq!(js_number(123456.789), "123456.789");
        assert_eq!(js_number(4.35), "4.35");
        assert_eq!(js_number(9007199254740992.0), "9007199254740992");
        assert_eq!(js_number(1.2345678901234568e29), "1.2345678901234568e+29");
        assert_eq!(js_number(f64::INFINITY), "Infinity");
        assert_eq!(js_number(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn 들여쓴_json은_빈_묶음과_큰_수와_음의_영을_stringify처럼_쓴다() {
        let value: Value = serde_json::from_str(
            r#"{"a":[],"b":{},"c":[1,{"d":null}],"e":"é \"\\","f":1.0,"g":1e21,"h":-0,"i":[[]]}"#,
        )
        .unwrap();
        assert_eq!(
            stringify_pretty(&value),
            "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    1,\n    {\n      \"d\": null\n    }\n  ],\n  \"e\": \"é\u{2028}\\\"\\\\\",\n  \"f\": 1,\n  \"g\": 1e+21,\n  \"h\": 0,\n  \"i\": [\n    []\n  ]\n}"
        );
        // 정수처럼 생긴 키는 자바스크립트 객체에서 앞으로 온다.
        let value: Value = serde_json::from_str(r#"{"b":1,"10":2,"2":3}"#).unwrap();
        assert_eq!(
            stringify_pretty(&value),
            "{\n  \"2\": 3,\n  \"10\": 2,\n  \"b\": 1\n}"
        );
        assert_eq!(stringify_pretty(&json!([])), "[]");
        assert_eq!(stringify_pretty(&json!(true)), "true");
    }

    #[test]
    fn 배열을_문자열로_바꾸면_안쪽_null은_비고_객체는_object다() {
        assert_eq!(
            js_string(Some(&json!([1, [2, null], { "a": 1 }, true]))),
            "1,2,,[object Object],true"
        );
        assert_eq!(js_string(Some(&json!([]))), "");
        assert_eq!(js_string(Some(&json!([null]))), "");
        assert_eq!(js_string(Some(&json!(-0.0))), "0");
    }

    #[test]
    fn 정수인_숫자는_정수로_저장한다() {
        assert_eq!(number_value(100.0).to_string(), "100");
        assert_eq!(number_value(0.5).to_string(), "0.5");
        assert_eq!(number_value(f64::NAN), Value::Null);
    }

    #[test]
    fn utf16_단위로_앞부분을_자른다() {
        assert_eq!(utf16_prefix("abc", 5), "abc");
        assert_eq!(utf16_prefix("abcd", 3), "abc");
        assert_eq!(utf16_prefix("가나다", 2), "가나");
        // 이모지는 두 단위다: 가운데를 자르면 통째로 빠진다.
        assert_eq!(utf16_prefix("a😀b", 2), "a");
        assert_eq!(utf16_prefix("a😀b", 3), "a😀");
    }
}
