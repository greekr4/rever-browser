//! Script, endpoint and request analysis over captured traffic, ported from src/main/mcp

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde_json::{Map, Number, Value};

pub mod codegen;
pub mod endpoint_template;
pub mod endpoints;
pub mod grep_worker;
pub mod har_build;
pub mod request_diff;
pub mod script_analysis;
pub mod stats;
pub mod token_decode;
pub mod triggered_requests;

// ── JavaScript semantics shared by the ported modules ───────────────────────

/// JavaScript's `\s` as a regex class: Rust's differs by U+FEFF (JS only) and
/// U+0085 (Rust only).
const JS_SPACE_CLASS: &str = r"[\s\x{feff}--\x{85}]";

/// What JavaScript's `\s` and `trim()` call whitespace.
fn is_js_space(c: char) -> bool {
    c == '\u{feff}' || (c.is_whitespace() && c != '\u{85}')
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// JavaScript's `string.length`.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Byte length of the longest prefix that fits in `units` UTF-16 code units:
/// where JavaScript's `slice(0, units)` cuts. A surrogate pair split by the cut
/// is left out whole, since a Rust string can't hold the lone half.
fn utf16_prefix_len(text: &str, units: usize) -> usize {
    let mut seen = 0;
    text.char_indices()
        .find(|(_, c)| {
            seen += c.len_utf16();
            seen > units
        })
        .map_or(text.len(), |(at, _)| at)
}

/// Byte offset where the longest suffix that fits in `units` UTF-16 code units
/// starts: where JavaScript's `slice(-units)` cuts (a split pair is left out).
fn utf16_suffix_start(text: &str, units: usize) -> usize {
    let mut seen = 0;
    text.char_indices()
        .rev()
        .take_while(|(_, c)| {
            seen += c.len_utf16();
            seen <= units
        })
        .last()
        .map_or(text.len(), |(at, _)| at)
}

/// An "array index" property name: JavaScript lists these first, in numeric order.
fn array_index(key: &str) -> Option<u32> {
    let n: u32 = key.parse().ok()?;
    (n != u32::MAX && n.to_string() == key).then_some(n)
}

/// Reorders keys the way a JavaScript object enumerates them: array indices
/// ascending, then the rest in insertion order.
fn js_key_order(map: Map<String, Value>) -> Map<String, Value> {
    if !map.keys().any(|k| array_index(k).is_some()) {
        return map;
    }
    let (mut indices, rest): (Vec<_>, Vec<_>) =
        map.into_iter().partition(|(k, _)| array_index(k).is_some());
    indices.sort_by_key(|(k, _)| array_index(k));
    indices.into_iter().chain(rest).collect()
}

/// `Number.prototype.toString()`: the shortest digits that round-trip, written
/// out in full from 1e-7 up to 1e21 and in exponent notation outside that.
fn js_number(f: f64) -> String {
    if f == 0.0 {
        return "0".into();
    }
    if !f.is_finite() {
        // What JSON.stringify writes for NaN and the infinities.
        return "null".into();
    }
    // `{:e}` gives those same shortest digits, as "d.ddde±x".
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits = mantissa.replace('.', "");
    // Position of the decimal point relative to the first digit.
    let point = exp.parse::<i64>().unwrap_or(0) + 1;
    let count = digits.len() as i64;
    let zeros = |n: i64| "0".repeat(usize::try_from(n).unwrap_or(0));
    let text = if (count..=21).contains(&point) {
        format!("{digits}{}", zeros(point - count))
    } else if (1..=21).contains(&point) {
        let (int, frac) = digits
            .split_at_checked(point as usize)
            .unwrap_or((&digits, ""));
        format!("{int}.{frac}")
    } else if (-5..=0).contains(&point) {
        format!("0.{}{digits}", zeros(-point))
    } else {
        let (first, rest) = digits.split_at_checked(1).unwrap_or((&digits, ""));
        let dot = if rest.is_empty() { "" } else { "." };
        let sign = if point > 0 { '+' } else { '-' };
        format!("{first}{dot}{rest}e{sign}{}", (point - 1).abs())
    };
    if f < 0.0 { format!("-{text}") } else { text }
}

/// `JSON.parse`, with the result shaped as JavaScript would hold it: every
/// number is a double (so `1.0` and `1e3` print as `1` and `1000`, and an
/// integer past 2^53 is rounded as JavaScript rounds it) and object keys
/// enumerate in JavaScript order. None where JSON.parse throws.
fn json_parse(text: &str) -> Option<Value> {
    fn number(n: Number) -> Number {
        let Some(f) = n.as_f64() else {
            return n;
        };
        if f.fract() != 0.0 {
            return n;
        }
        // The integer JavaScript prints for this double, while `Number` can
        // hold it; past that it stays a double and prints with an exponent.
        let text = js_number(f);
        if let Ok(i) = text.parse::<i64>() {
            return Number::from(i);
        }
        if let Ok(u) = text.parse::<u64>() {
            return Number::from(u);
        }
        Number::from_f64(f).unwrap_or(n)
    }
    fn normalize(value: Value) -> Value {
        match value {
            Value::Number(n) => Value::Number(number(n)),
            Value::Array(items) => Value::Array(items.into_iter().map(normalize).collect()),
            Value::Object(map) => Value::Object(js_key_order(
                map.into_iter().map(|(k, v)| (k, normalize(v))).collect(),
            )),
            other => other,
        }
    }
    serde_json::from_str(text).ok().map(normalize)
}

/// `JSON.stringify` of a parsed value: numbers are written as JavaScript
/// writes them, which serde_json does not do below 1e-5 or from 1e16 up.
fn json_stringify(value: &Value) -> String {
    match value {
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), js_number),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(json_stringify).collect();
            format!("[{}]", items.join(","))
        }
        Value::Object(map) => {
            let members: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}:{}", json_string(k), json_stringify(v)))
                .collect();
            format!("{{{}}}", members.join(","))
        }
        other => other.to_string(),
    }
}

/// JavaScript truthiness of a parsed JSON value.
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `JSON.stringify` of a string.
fn json_string(text: &str) -> String {
    Value::from(text).to_string()
}

// Everything encodeURIComponent leaves alone besides letters and digits.
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

fn encode_uri_component(text: &str) -> String {
    utf8_percent_encode(text, URI_COMPONENT).to_string()
}

/// `decodeURIComponent`; None where JavaScript throws URIError (a `%` without
/// two hex digits, or escapes that are not UTF-8).
fn decode_uri_component(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let malformed = bytes.iter().enumerate().any(|(i, b)| {
        *b == b'%'
            && !(bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
    });
    if malformed {
        return None;
    }
    percent_decode_str(text)
        .decode_utf8()
        .ok()
        .map(|s| s.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 객체_키는_자바스크립트처럼_숫자_키가_먼저_온다() {
        let parsed = json_parse(r#"{"b":1,"10":2,"2":3,"01":4}"#).unwrap();
        assert_eq!(parsed.to_string(), r#"{"2":3,"10":2,"b":1,"01":4}"#);
    }

    #[test]
    fn 숫자는_자바스크립트_double처럼_다시_쓴다() {
        let parsed = json_parse(r#"[1.0,1e3,-0,0.5,"1.0"]"#).unwrap();
        assert_eq!(parsed.to_string(), r#"[1,1000,0,0.5,"1.0"]"#);
        assert_eq!(json_parse("{oops"), None);
    }

    #[test]
    fn 숫자를_자바스크립트와_같은_글자로_쓴다() {
        // Reference values from String(JSON.parse(text)) under bun.
        let cases = [
            ("12345678901234567890", "12345678901234567000"),
            ("9007199254740993", "9007199254740992"),
            ("18446744073709551615", "18446744073709552000"),
            ("-9223372036854775809", "-9223372036854776000"),
            ("1e16", "10000000000000000"),
            ("-1e20", "-100000000000000000000"),
            ("123456789012345680000", "123456789012345680000"),
            ("9.999999999999999e20", "999999999999999900000"),
            ("999999999999999999999", "1e+21"),
            ("1e21", "1e+21"),
            ("1.5e300", "1.5e+300"),
            ("1.7976931348623157e308", "1.7976931348623157e+308"),
            ("0.000001", "0.000001"),
            ("1.2e-6", "0.0000012"),
            ("-0.0000012", "-0.0000012"),
            ("0.00001234", "0.00001234"),
            ("1e-7", "1e-7"),
            ("1.2e-7", "1.2e-7"),
            ("0.5e-6", "5e-7"),
            ("123e-20", "1.23e-18"),
            ("5e-324", "5e-324"),
            ("123456.789", "123456.789"),
            ("25e-1", "2.5"),
            ("-1.50", "-1.5"),
            ("1.0", "1"),
            ("-0", "0"),
            // Rounded to the nearest double, not one next to it.
            ("0.12345678901234567", "0.12345678901234566"),
            ("37.566535012345678", "37.566535012345675"),
        ];
        for (text, expected) in cases {
            let parsed = json_parse(text).unwrap();
            assert_eq!(json_stringify(&parsed), expected, "{text}");
        }
        assert_eq!(js_number(f64::NAN), "null");
        assert_eq!(js_number(f64::INFINITY), "null");
    }

    #[test]
    fn 큰_정수는_자바스크립트처럼_double로_반올림해_담는다() {
        // The value itself is rounded, so whoever serializes it prints what
        // JavaScript prints and two ids that differ past 2^53 compare equal.
        assert_eq!(
            json_parse(r#"{"id":12345678901234567890,"n":-9007199254740993}"#)
                .unwrap()
                .to_string(),
            r#"{"id":12345678901234567000,"n":-9007199254740992}"#
        );
        assert_eq!(
            json_parse("12345678901234567890"),
            json_parse("12345678901234567891")
        );
        assert_eq!(
            json_stringify(
                &json_parse(r#"{"a":[1e21,{"b":0.000001}],"c":"1e-6","d":null,"e":true}"#).unwrap()
            ),
            r#"{"a":[1e+21,{"b":0.000001}],"c":"1e-6","d":null,"e":true}"#
        );
    }

    #[test]
    fn uri_컴포넌트_인코딩은_자바스크립트와_같은_문자를_남긴다() {
        assert_eq!(
            encode_uri_component("a b&c=d/é-_.!~*'()"),
            "a%20b%26c%3Dd%2F%C3%A9-_.!~*'()"
        );
        assert_eq!(
            decode_uri_component("a%20b%C3%A9+c").as_deref(),
            Some("a bé+c")
        );
    }

    #[test]
    fn 잘못된_퍼센트_인코딩은_디코딩하지_않는다() {
        assert_eq!(decode_uri_component("100%"), None);
        assert_eq!(decode_uri_component("%zz"), None);
        // A lone continuation byte is not UTF-8: JavaScript throws URIError.
        assert_eq!(decode_uri_component("%80"), None);
        assert_eq!(decode_uri_component("%C3"), None);
    }

    #[test]
    fn utf16_단위로_앞뒤를_자른다() {
        // "😀" is two UTF-16 units and four bytes.
        let text = "a😀b";
        assert_eq!(utf16_len(text), 4);
        assert_eq!(utf16_prefix_len(text, 1), 1);
        assert_eq!(utf16_prefix_len(text, 2), 1);
        assert_eq!(utf16_prefix_len(text, 3), 5);
        assert_eq!(utf16_suffix_start(text, 1), 5);
        assert_eq!(utf16_suffix_start(text, 2), 5);
        assert_eq!(utf16_suffix_start(text, 3), 1);
        assert_eq!(utf16_suffix_start(text, 99), 0);
    }

    // ── the secret-scan cases of src/main/mcp/pure-helpers.test.ts ──────────
    // secret-scan.ts is ported in rever-core, whose tests follow its sibling
    // secret-scan.test.ts; these three live in the shared helper suite, and the
    // masking here relies on what they pin down.

    #[test]
    fn 긴_값은_앞뒤_네_글자와_길이만_남기고_가린다() {
        use rever_core::secret_scan::mask;
        assert_eq!(mask("short"), "****");
        assert_eq!(mask("AKIA1234567890ABCDEF"), "AKIA…CDEF (20 chars)");
    }

    #[test]
    fn aws_키와_jwt를_찾고_출처를_표시한다() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghij";
        let hits = rever_core::secret_scan::scan_text(
            &format!("token=AKIAIOSFODNN7EXAMPLE and {jwt}"),
            "response-body",
        );
        let types: Vec<&str> = hits.iter().map(|h| h.kind.as_str()).collect();
        assert!(types.contains(&"aws-access-key-id"), "{types:?}");
        assert!(types.contains(&"jwt"), "{types:?}");
        assert!(hits.iter().all(|h| h.location == "response-body"));
    }

    #[test]
    fn 민감한_요청_헤더를_이름으로_보고한다() {
        let req = rever_core::traffic::StoredRequest {
            request_id: "r1".into(),
            url: "https://a.com".into(),
            host: "a.com".into(),
            request_headers: Some(
                [(
                    "Authorization".to_owned(),
                    "Bearer abcdefghijklmnop".to_owned(),
                )]
                .into(),
            ),
            ..Default::default()
        };
        let hits = rever_core::secret_scan::scan_request(&req);
        assert!(hits.iter().any(|h| h.kind == "header:authorization"));
    }

    #[test]
    fn 공백_판정은_자바스크립트_기준이다() {
        assert_eq!(js_trim("\u{feff} x \u{a0}"), "x");
        assert_eq!(js_trim("\u{85}x"), "\u{85}x");
        assert!(is_truthy(&Value::from("0")));
        assert!(!is_truthy(&Value::from(0)));
        assert!(!is_truthy(&Value::from("")));
        assert!(!is_truthy(&Value::Null));
    }
}
