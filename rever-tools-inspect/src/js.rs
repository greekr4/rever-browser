//! JavaScript semantics the ported tools share, so their output text equals
//! what the TypeScript printed: UTF-16 lengths, `JSON.stringify` number
//! formatting, truthiness, `String(value)`, and Node's lenient `Buffer.from`.

use serde_json::{Map, Value};

/// JavaScript's `\s` as a regex class: Rust's differs by U+FEFF (JS only) and
/// U+0085 (Rust only).
pub(crate) const JS_SPACE: &str = r"[\s\x{feff}--\x{85}]";

fn is_js_space(c: char) -> bool {
    c == '\u{feff}' || (c.is_whitespace() && c != '\u{85}')
}

/// `String.prototype.trim()`.
pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

/// JavaScript's `string.length`.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `text.slice(0, units)`. A surrogate pair split by the cut is left out
/// whole, since a Rust string can't hold the lone half.
pub(crate) fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut seen = 0;
    let end = text
        .char_indices()
        .find(|(_, c)| {
            seen += c.len_utf16();
            seen > units
        })
        .map_or(text.len(), |(at, _)| at);
    text.get(..end).unwrap_or(text)
}

/// `Number.prototype.toString()`: the shortest digits that round-trip, written
/// out in full from 1e-7 up to 1e21 and in exponent notation outside that.
pub(crate) fn js_number(f: f64) -> String {
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

/// `JSON.stringify(value, null, 2)` when `pretty`, `JSON.stringify(value)`
/// otherwise. Every number is written as the double JavaScript would hold:
/// serde_json prints `1.0`, `1e16` and integers past 2^53 differently.
pub(crate) fn json_stringify(value: &Value, pretty: bool) -> String {
    let mut out = String::new();
    write_json(value, pretty, 0, &mut out);
    out
}

fn write_json(value: &Value, pretty: bool, depth: usize, out: &mut String) {
    let line = |out: &mut String, depth: usize| {
        if pretty {
            out.push('\n');
            out.push_str(&"  ".repeat(depth));
        }
    };
    match value {
        Value::Number(n) => match n.as_f64() {
            Some(f) => out.push_str(&js_number(f)),
            None => out.push_str(&n.to_string()),
        },
        Value::Array(items) if !items.is_empty() => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                line(out, depth + 1);
                write_json(item, pretty, depth + 1, out);
            }
            line(out, depth);
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() => {
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                line(out, depth + 1);
                out.push_str(&Value::from(key.as_str()).to_string());
                out.push_str(if pretty { ": " } else { ":" });
                write_json(item, pretty, depth + 1, out);
            }
            line(out, depth);
            out.push('}');
        }
        // Strings, booleans, null and the empty containers print the same in both.
        other => out.push_str(&other.to_string()),
    }
}

/// `JSON.stringify` of a string: a lone surrogate is written as a `\uXXXX` escape.
pub(crate) fn json_string(input: &[u16]) -> String {
    let mut out = String::from("\"");
    for c in char::decode_utf16(input.iter().copied()) {
        match c {
            Ok('"') => out.push_str("\\\""),
            Ok('\\') => out.push_str("\\\\"),
            Ok('\u{8}') => out.push_str("\\b"),
            Ok('\u{c}') => out.push_str("\\f"),
            Ok('\n') => out.push_str("\\n"),
            Ok('\r') => out.push_str("\\r"),
            Ok('\t') => out.push_str("\\t"),
            Ok(c) if c < ' ' => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            Ok(c) => out.push(c),
            Err(lone) => out.push_str(&format!("\\u{:04x}", lone.unpaired_surrogate())),
        }
    }
    out.push('"');
    out
}

/// An "array index" property name: JavaScript lists these first, in numeric order.
fn array_index(key: &str) -> Option<u32> {
    let n: u32 = key.parse().ok()?;
    (n != u32::MAX && n.to_string() == key).then_some(n)
}

pub(crate) fn js_key_order(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(js_key_order).collect()),
        Value::Object(map) => {
            let entries = map.into_iter().map(|(k, v)| (k, js_key_order(v)));
            let (mut indices, rest): (Vec<_>, Vec<_>) =
                entries.partition(|(k, _)| array_index(k).is_some());
            indices.sort_by_key(|(k, _)| array_index(k));
            Value::Object(indices.into_iter().chain(rest).collect::<Map<_, _>>())
        }
        other => other,
    }
}

/// `JSON.parse`, with object keys in the order JavaScript enumerates them.
/// Numbers stay as parsed: [`json_stringify`] and [`to_js_string`] read them
/// as doubles.
pub(crate) fn json_parse(text: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(text).map(js_key_order)
}

/// JavaScript truthiness; `None` is `undefined`.
pub(crate) fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// `String(value)` — also what a template literal and `Array.prototype.join`
/// make of a value; `None` is `undefined`.
pub(crate) fn to_js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.as_f64().map_or_else(|| n.to_string(), js_number),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => js_join(items.iter().map(Some), ","),
        Some(Value::Object(_)) => "[object Object]".into(),
    }
}

/// `Array.prototype.join`: `null` and `undefined` elements become empty strings.
pub(crate) fn js_join<'a>(
    items: impl Iterator<Item = Option<&'a Value>>,
    separator: &str,
) -> String {
    let parts: Vec<String> = items
        .map(|item| match item {
            None | Some(Value::Null) => String::new(),
            other => to_js_string(other),
        })
        .collect();
    parts.join(separator)
}

/// `Buffer.from(text, 'binary')`: the low byte of each UTF-16 code unit.
pub(crate) fn latin1_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16().map(|unit| unit as u8).collect()
}

/// `Buffer.from(text, 'base64')` (and `'base64url'`, which Node decodes the
/// same way): both alphabets, characters outside them skipped, decoding
/// stopped at the first `=`. It never fails. Node reads only the low byte of
/// each code unit, so a non-ASCII character counts as whatever that byte is.
pub(crate) fn base64_bytes(units: impl IntoIterator<Item = u16>) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut bits, mut pending) = (0u32, 0u32);
    for unit in units {
        let c = unit as u8;
        let sextet = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        bits = (bits << 6) | u32::from(sextet);
        pending += 6;
        if pending >= 8 {
            pending -= 8;
            out.push((bits >> pending) as u8);
        }
    }
    out
}

/// `Buffer.from(text, 'hex')`: whole pairs up to the first one that is not
/// hex. Like base64, Node reads only the low byte of each code unit.
pub(crate) fn hex_bytes(units: impl IntoIterator<Item = u16>) -> Vec<u8> {
    let nibble = |unit: u16| char::from(unit as u8).to_digit(16);
    let mut units = units.into_iter();
    let mut out = Vec::new();
    while let (Some(high), Some(low)) = (units.next(), units.next()) {
        let (Some(high), Some(low)) = (nibble(high), nibble(low)) else {
            break;
        };
        out.push((high << 4 | low) as u8);
    }
    out
}

/// `buf.toString('hex')`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn 숫자는_자바스크립트가_찍는_모양으로_쓴다() {
        // Expected strings are what node prints for String(n).
        assert_eq!(js_number(1.0), "1");
        assert_eq!(js_number(-0.0), "0");
        assert_eq!(js_number(0.1), "0.1");
        assert_eq!(js_number(1e21), "1e+21");
        assert_eq!(js_number(1e16), "10000000000000000");
        assert_eq!(js_number(1e-7), "1e-7");
        assert_eq!(js_number(0.000001), "0.000001");
        assert_eq!(js_number(-2.5), "-2.5");
        assert_eq!(js_number(1.2345678901234568e29), "1.2345678901234568e+29");
        assert_eq!(js_number(5e-324), "5e-324");
        assert_eq!(js_number(f64::NAN), "null");
    }

    #[test]
    fn json은_들여쓰기_두_칸과_한_줄_두_가지로_쓴다() {
        let value = json!({ "a": [], "b": {}, "c": [1, 2.0, { "d": "한\"\n" }], "e": null });
        assert_eq!(
            json_stringify(&value, true),
            "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    1,\n    2,\n    {\n      \"d\": \"한\\\"\\n\"\n    }\n  ],\n  \"e\": null\n}"
        );
        assert_eq!(
            json_stringify(&value, false),
            "{\"a\":[],\"b\":{},\"c\":[1,2,{\"d\":\"한\\\"\\n\"}],\"e\":null}"
        );
    }

    #[test]
    fn 안전한_정수_범위를_넘는_수는_자바스크립트처럼_반올림된다() {
        let value = json_parse("[9007199254740993, 1e3, 1.50, 123456789012345678901234567890]");
        assert_eq!(
            value.ok().map(|v| json_stringify(&v, false)),
            Some("[9007199254740992,1000,1.5,1.2345678901234568e+29]".to_owned())
        );
    }

    #[test]
    fn 파싱한_객체의_정수_키는_숫자_순서로_앞에_온다() {
        let value = json_parse(r#"{"b":1,"10":2,"2":{"z":0,"1":0},"01":3}"#);
        assert_eq!(
            value.ok().map(|v| json_stringify(&v, false)),
            Some(r#"{"2":{"1":0,"z":0},"10":2,"b":1,"01":3}"#.to_owned())
        );
        assert!(json_parse("{").is_err());
    }

    #[test]
    fn 참_거짓_판정은_자바스크립트와_같다() {
        for falsy in [json!(null), json!(false), json!(0), json!(0.0), json!("")] {
            assert!(!is_truthy(Some(&falsy)), "{falsy}");
        }
        for truthy in [json!(true), json!(-1), json!("0"), json!([]), json!({})] {
            assert!(is_truthy(Some(&truthy)), "{truthy}");
        }
        assert!(!is_truthy(None));
    }

    #[test]
    fn 문자열_변환은_자바스크립트의_string_함수와_같다() {
        // Checked against node: String(JSON.parse(...)).
        let value = json!([1, [2, null, [3]], { "a": 1 }, "x", true]);
        assert_eq!(to_js_string(Some(&value)), "1,2,,3,[object Object],x,true");
        assert_eq!(to_js_string(Some(&json!(1e21))), "1e+21");
        assert_eq!(to_js_string(Some(&json!(null))), "null");
        assert_eq!(to_js_string(None), "undefined");
        assert_eq!(to_js_string(Some(&json!({}))), "[object Object]");
    }

    #[test]
    fn 길이와_자르기는_utf16_단위로_센다() {
        assert_eq!(utf16_len("가😀a"), 4);
        assert_eq!(utf16_prefix("가😀a", 1), "가");
        // Cutting inside the pair leaves the whole emoji out.
        assert_eq!(utf16_prefix("가😀a", 2), "가");
        assert_eq!(utf16_prefix("가😀a", 3), "가😀");
        assert_eq!(utf16_prefix("abc", 99), "abc");
    }

    #[test]
    fn trim은_자바스크립트의_공백만_잘라낸다() {
        assert_eq!(js_trim("\u{feff} \t\na\u{a0}\u{2028}"), "a");
        // U+0085 is whitespace for Rust but not for JavaScript.
        assert_eq!(js_trim("\u{85}a\u{85}"), "\u{85}a\u{85}");
    }

    #[test]
    fn base64는_노드처럼_너그럽게_읽는다() {
        // Every expectation is what node's Buffer.from(s, 'base64') returns.
        assert_eq!(base64_bytes(units("aGVsbG8=")), b"hello");
        assert_eq!(base64_bytes(units("aGVs bG8")), b"hello");
        assert_eq!(base64_bytes(units("aG!Vs@bG8")), b"hello");
        assert_eq!(base64_bytes(units("aGVs=bG8=")), b"hel");
        assert_eq!(base64_bytes(units("=aGVs")), b"");
        assert_eq!(base64_bytes(units("a")), b"");
        assert_eq!(base64_bytes(units("aG")), b"h");
        assert_eq!(base64_bytes(units("-_-_")), [251, 255, 191]);
        assert_eq!(base64_bytes(units("+/+/")), [251, 255, 191]);
        // U+0141 is read as its low byte 0x41 ("A"); the emoji's first unit
        // ends in 0x3D ("="), which stops the decoding.
        assert_eq!(
            base64_bytes(units("aGVs\u{141}bG8")),
            [104, 101, 108, 1, 177, 188]
        );
        assert_eq!(base64_bytes(units("aGVs😀bG8")), b"hel");
    }

    #[test]
    fn hex는_처음으로_잘못된_쌍에서_멈춘다() {
        // Every expectation is what node's Buffer.from(s, 'hex') returns.
        assert_eq!(hex_bytes(units("AbCd")), [171, 205]);
        assert_eq!(hex_bytes(units("abc")), [171]);
        assert_eq!(hex_bytes(units("zz")), b"");
        assert_eq!(hex_bytes(units("61zz62")), b"a");
        assert_eq!(hex_bytes(units("61 62")), b"a");
        assert_eq!(hex_bytes(units("6162z")), b"ab");
        assert_eq!(hex_bytes(units("0x6162")), b"");
        assert_eq!(hex_bytes(units("61\u{141}\u{141}62")), [97, 170, 98]);
        assert_eq!(hex(&[0, 171, 255]), "00abff");
    }

    #[test]
    fn binary는_코드_단위의_하위_바이트만_남긴다() {
        assert_eq!(latin1_bytes("a\u{e9}\u{141}😀"), [97, 233, 65, 61, 0]);
    }
}
