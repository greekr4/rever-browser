//! Port of `src/renderer/src/lib/format-json.ts` — pretty-printing tool output
//! for display.

use serde_json::{Map, Number, Value};

use crate::js_trim;

pub fn try_pretty(text: &str) -> String {
    let trimmed = js_trim(text);
    if ((trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']')))
        && let Ok(value) = serde_json::from_str::<Value>(trimmed)
    {
        return stringify_pretty(&value);
    }
    // fallthrough: not JSON-shaped, or it did not parse
    text.to_owned()
}

/// `None` is JavaScript's `undefined`; it and JSON `null` both give "".
pub fn format_output(output: Option<&Value>) -> String {
    let output = match output {
        None | Some(Value::Null) => return String::new(),
        Some(Value::String(text)) => return try_pretty(text),
        Some(other) => other,
    };

    let content = match output {
        Value::Array(items) => Some(items),
        other => other.get("content").and_then(Value::as_array),
    };
    if let Some(items) = content {
        // Only when every block is a text block; one other block shows the raw JSON.
        let texts: Option<Vec<String>> = items
            .iter()
            .map(|c| {
                let is_text = c.get("type").and_then(Value::as_str) == Some("text");
                let text = c.get("text").and_then(Value::as_str)?;
                is_text.then(|| try_pretty(text))
            })
            .collect();
        if let Some(texts) = texts.filter(|t| !t.is_empty()) {
            return texts.join("\n\n");
        }
    }

    stringify_pretty(output)
}

/// `JSON.stringify(value, null, 2)`.
fn stringify_pretty(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, 0);
    out
}

fn indent(out: &mut String, depth: usize) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_string(out: &mut String, text: &str) {
    // serde_json escapes a string exactly as JSON.stringify does; a plain
    // string always serializes.
    out.push_str(&serde_json::to_string(text).unwrap_or_default());
}

fn write_value(out: &mut String, value: &Value, depth: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&js_number(n)),
        Value::String(s) => write_string(out, s),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                indent(out, depth + 1);
                write_value(out, item, depth + 1);
            }
            indent(out, depth);
            out.push(']');
        }
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in js_key_order(map).into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                indent(out, depth + 1);
                write_string(out, key);
                out.push_str(": ");
                write_value(out, item, depth + 1);
            }
            indent(out, depth);
            out.push('}');
        }
    }
}

/// A key JavaScript treats as an array index: canonical decimal digits below
/// 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty());
    if !canonical || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|&n| n != u32::MAX)
}

/// The order JavaScript lists an object's keys in: array-index keys first, in
/// ascending numeric order, then the rest in insertion order.
fn js_key_order(map: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut indices = Vec::new();
    let mut rest = Vec::new();
    for entry in map {
        match array_index(entry.0) {
            Some(n) => indices.push((n, entry)),
            None => rest.push(entry),
        }
    }
    indices.sort_by_key(|(n, _)| *n);
    indices
        .into_iter()
        .map(|(_, entry)| entry)
        .chain(rest)
        .collect()
}

/// The digits and decimal exponent of `{:e}` output: "1.25e3" gives ("125", 3).
fn split_scientific(text: &str) -> Option<(String, i32)> {
    let (mantissa, exponent) = text.split_once('e')?;
    let digits = mantissa.chars().filter(char::is_ascii_digit).collect();
    Some((digits, exponent.parse().ok()?))
}

/// The shortest digits that read back as `x` (positive and finite), with the
/// decimal exponent of the first digit.
///
/// Rust's exponent form already gives these, except when `x` lies exactly
/// halfway between the two shortest candidates: Rust then rounds the last
/// digit up, JavaScript rounds it to even (932404050728330.25 prints as
/// …330.2 there and …330.3 here).
fn shortest_digits(x: f64) -> Option<(String, i32)> {
    let (digits, exponent) = split_scientific(&format!("{x:e}"))?;
    let k = digits.len();
    // Two digits further than the tie digit: a cheap filter, since nearly no
    // number continues with exactly "500".
    let (probe, probe_exponent) = split_scientific(&format!("{:.*e}", k + 2, x))?;
    let Some((lower, "500")) = probe.split_at_checked(k) else {
        return Some((digits, exponent));
    };
    let lower_is_even = lower.bytes().last().is_some_and(|b| b % 2 == 0);
    if probe_exponent != exponent || lower == digits || !lower_is_even {
        return Some((digits, exponent));
    }
    // A double has at most 767 significant decimal digits, so this is its
    // exact value: a tie only if nothing but zeros follows the 5.
    let (exact, _) = split_scientific(&format!("{x:.800e}"))?;
    let is_tie = exact
        .get(k + 1..)
        .is_some_and(|rest| rest.bytes().all(|b| b == b'0'));
    // The even candidate still has to read back as the same double.
    let reads_back = lower
        .split_at_checked(1)
        .and_then(|(first, more)| format!("{first}.{more}e{exponent}").parse::<f64>().ok())
        == Some(x);
    if is_tie && reads_back {
        return Some((lower.to_owned(), exponent));
    }
    Some((digits, exponent))
}

/// A number as JavaScript prints it. Every JSON number is a double there, so
/// `1.0` prints as `1`, integers past 2^53 lose their low digits, and the
/// exponent form starts at 1e21 and below 1e-6.
fn js_number(number: &Number) -> String {
    let Some(x) = number.as_f64().filter(|x| x.is_finite()) else {
        return "null".to_owned();
    };
    if x == 0.0 {
        // Covers -0, which JSON.stringify prints without the sign.
        return "0".to_owned();
    }
    let Some((digits, exponent)) = shortest_digits(x.abs()) else {
        return x.to_string();
    };
    let k = digits.len() as i32;
    // The decimal point sits after the first `n` digits.
    let n = exponent + 1;
    let zeros = |count: i32| "0".repeat(count.max(0) as usize);
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", zeros(n - k))
    } else if 0 < n && n <= 21 {
        match digits.split_at_checked(n as usize) {
            Some((whole, fraction)) => format!("{whole}.{fraction}"),
            None => digits,
        }
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", zeros(-n))
    } else {
        let sign = if n > 0 { '+' } else { '-' };
        let power = (n - 1).abs();
        match digits.split_at_checked(1) {
            Some((first, "")) => format!("{first}e{sign}{power}"),
            Some((first, more)) => format!("{first}.{more}e{sign}{power}"),
            None => digits,
        }
    };
    if x < 0.0 { format!("-{body}") } else { body }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // --- format-json.test.ts: tryPretty ---

    // it('pretty-prints a valid JSON object')
    #[test]
    fn 유효한_json_객체를_보기_좋게_출력한다() {
        assert_eq!(
            try_pretty(r#"{"a":1,"b":2}"#),
            "{\n  \"a\": 1,\n  \"b\": 2\n}"
        );
    }

    // it('pretty-prints a valid JSON array')
    #[test]
    fn 유효한_json_배열을_보기_좋게_출력한다() {
        assert_eq!(try_pretty("[1,2]"), "[\n  1,\n  2\n]");
    }

    // it('tolerates surrounding whitespace')
    #[test]
    fn 앞뒤_공백은_무시한다() {
        assert_eq!(try_pretty("  {\"a\":1}  "), "{\n  \"a\": 1\n}");
    }

    // it('returns the original text when it is not JSON-shaped')
    #[test]
    fn json_모양이_아니면_원문을_그대로_돌려준다() {
        assert_eq!(try_pretty("hello world"), "hello world");
    }

    // it('returns the original text when JSON parse fails')
    #[test]
    fn json_파싱에_실패하면_원문을_그대로_돌려준다() {
        // Looks like an object but is malformed — must fall through untouched.
        assert_eq!(try_pretty("{not valid}"), "{not valid}");
    }

    // --- format-json.test.ts: formatOutput ---

    // it('returns an empty string for null/undefined')
    #[test]
    fn null과_undefined는_빈_문자열이다() {
        assert_eq!(format_output(Some(&Value::Null)), "");
        assert_eq!(format_output(None), "");
    }

    // it('pretty-prints a JSON string')
    #[test]
    fn json_문자열을_보기_좋게_출력한다() {
        assert_eq!(format_output(Some(&json!("{\"a\":1}"))), "{\n  \"a\": 1\n}");
    }

    // it('joins an array of text content blocks')
    #[test]
    fn 텍스트_콘텐츠_블록_배열을_이어_붙인다() {
        let out = json!([
            { "type": "text", "text": "first" },
            { "type": "text", "text": "{\"a\":1}" }
        ]);
        assert_eq!(format_output(Some(&out)), "first\n\n{\n  \"a\": 1\n}");
    }

    // it('reads a content array off an object')
    #[test]
    fn 객체의_content_배열을_읽는다() {
        let out = json!({ "content": [{ "type": "text", "text": "hello" }] });
        assert_eq!(format_output(Some(&out)), "hello");
    }

    // it('falls back to JSON.stringify for non-text shapes')
    #[test]
    fn 텍스트_모양이_아니면_json_stringify로_돌아간다() {
        let out = json!({ "foo": "bar" });
        assert_eq!(format_output(Some(&out)), "{\n  \"foo\": \"bar\"\n}");
    }

    // it('falls back to JSON.stringify when a content block is not text')
    #[test]
    fn 콘텐츠_블록이_텍스트가_아니면_json_stringify로_돌아간다() {
        let out = json!({ "content": [{ "type": "image", "data": "x" }] });
        // JSON.stringify(out, null, 2)
        assert_eq!(
            format_output(Some(&out)),
            "{\n  \"content\": [\n    {\n      \"type\": \"image\",\n      \"data\": \"x\"\n    }\n  ]\n}"
        );
    }

    // --- not covered by format-json.test.ts ---

    #[test]
    fn 실패한_경우_공백을_포함한_원문을_그대로_돌려준다() {
        assert_eq!(try_pretty("  {not valid}  "), "  {not valid}  ");
        assert_eq!(try_pretty(""), "");
        assert_eq!(try_pretty("{"), "{");
        assert_eq!(try_pretty("[1,2}"), "[1,2}");
        // Valid JSON, but not object- or array-shaped: left alone.
        assert_eq!(try_pretty("123"), "123");
        assert_eq!(try_pretty("\"{}\""), "\"{}\"");
        // Trailing data after the value is a parse error.
        assert_eq!(try_pretty("{} {}"), "{} {}");
    }

    #[test]
    fn json_parse만_받아들이는_입력은_패닉_없이_원문으로_남는다() {
        // Known gaps against JSON.parse, which formats all of these: serde_json
        // rejects a number past the double range (JavaScript reads Infinity and
        // prints null), a lone surrogate escape (a Rust string cannot hold
        // one), and nesting deeper than 127 levels. They fall through as text.
        let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
        for text in ["[1e400]", "{\"a\":-1e999}", "[\"\\ud800\"]", deep.as_str()] {
            assert_eq!(try_pretty(text), text);
        }
        // The deepest nesting that still parses is formatted, not cut off.
        let ok = format!("{}{}", "[".repeat(127), "]".repeat(127));
        assert_ne!(try_pretty(&ok), ok);
        // Underflow is not an error on either side: it reads as 0.
        assert_eq!(try_pretty("[1e-400]"), "[\n  0\n]");
    }

    #[test]
    fn 자바스크립트_trim이_지우는_bom과_nbsp도_공백으로_본다() {
        assert_eq!(try_pretty("\u{FEFF}{\"a\":1}\u{A0}"), "{\n  \"a\": 1\n}");
        // NEL is not whitespace for JavaScript, so the text does not start with "{".
        assert_eq!(try_pretty("\u{85}{\"a\":1}"), "\u{85}{\"a\":1}");
    }

    #[test]
    fn 빈_객체와_빈_배열은_한_줄로_출력하고_중첩은_두_칸씩_들여쓴다() {
        assert_eq!(try_pretty("{}"), "{}");
        assert_eq!(try_pretty("[ ]"), "[]");
        assert_eq!(
            try_pretty(r#"{"a":{"b":[],"c":{}},"d":[[1],{"e":null}]}"#),
            "{\n  \"a\": {\n    \"b\": [],\n    \"c\": {}\n  },\n  \"d\": [\n    [\n      1\n    ],\n    {\n      \"e\": null\n    }\n  ]\n}"
        );
    }

    #[test]
    fn 키는_들어온_순서를_유지하되_정수_키는_오름차순으로_앞에_온다() {
        assert_eq!(
            try_pretty(r#"{"b":1,"a":2}"#),
            "{\n  \"b\": 1,\n  \"a\": 2\n}"
        );
        // JSON.stringify(JSON.parse('{"b":1,"10":2,"2":3,"01":4,"-1":5}'), null, 2)
        assert_eq!(
            try_pretty(r#"{"b":1,"10":2,"2":3,"01":4,"-1":5}"#),
            "{\n  \"2\": 3,\n  \"10\": 2,\n  \"b\": 1,\n  \"01\": 4,\n  \"-1\": 5\n}"
        );
        // 4294967294 is the largest array index; 4294967295 is an ordinary key.
        assert_eq!(
            try_pretty(r#"{"4294967295":1,"x":2,"4294967294":3}"#),
            "{\n  \"4294967294\": 3,\n  \"4294967295\": 1,\n  \"x\": 2\n}"
        );
    }

    #[test]
    fn 중복_키는_마지막_값이_처음_자리에_남는다() {
        assert_eq!(
            try_pretty(r#"{"a":1,"b":2,"a":3}"#),
            "{\n  \"a\": 3,\n  \"b\": 2\n}"
        );
    }

    #[test]
    fn 숫자는_자바스크립트_표기로_출력한다() {
        // Each expected value is JSON.stringify(JSON.parse(input)).
        for (input, expected) in [
            ("1.0", "1"),
            ("-0", "0"),
            ("-0.0", "0"),
            ("1.50", "1.5"),
            ("100", "100"),
            ("1E2", "100"),
            ("-12.5e1", "-125"),
            ("0.1", "0.1"),
            ("0.30000000000000004", "0.30000000000000004"),
            ("0.000001", "0.000001"),
            ("0.0000001", "1e-7"),
            ("1.5e-7", "1.5e-7"),
            ("123456789012345680000", "123456789012345680000"),
            ("1e21", "1e+21"),
            ("1.5e21", "1.5e+21"),
            ("1e300", "1e+300"),
            ("5e-324", "5e-324"),
            ("9007199254740993", "9007199254740992"),
            ("12345678901234567890", "12345678901234567000"),
            ("-9223372036854775808", "-9223372036854776000"),
            ("18446744073709551615", "18446744073709552000"),
            ("123456789012345678901234567890", "1.2345678901234568e+29"),
            ("3.141592653589793", "3.141592653589793"),
            ("1.7976931348623157e308", "1.7976931348623157e+308"),
            // Exactly halfway between two shortest candidates: JavaScript
            // takes the even one, …330.2, where Rust's own printing gives …330.3.
            ("932404050728330.25", "932404050728330.2"),
            ("-1336450365114504.25", "-1336450365114504.2"),
            // Halfway with an odd digit below rounds up in both.
            ("932404050728330.75", "932404050728330.8"),
        ] {
            assert_eq!(
                try_pretty(&format!("[{input}]")),
                format!("[\n  {expected}\n]"),
                "{input}"
            );
        }
    }

    #[test]
    fn 문자열은_json_stringify와_같게_이스케이프한다() {
        // JSON.stringify(JSON.parse(input), null, 2) keeps non-ASCII text and
        // re-escapes only quotes, backslashes and control characters.
        assert_eq!(
            try_pretty(r#"["Aé가","a\"b\\c\/d","\n\t\u0001\u001f\u007f","😀"]"#),
            "[\n  \"Aé가\",\n  \"a\\\"b\\\\c/d\",\n  \"\\n\\t\\u0001\\u001f\u{7f}\",\n  \"😀\"\n]"
        );
        assert_eq!(
            try_pretty("{\"k\\\"ey\":true}"),
            "{\n  \"k\\\"ey\": true\n}"
        );
    }

    #[test]
    fn 문자열이_아닌_원시값은_json_그대로_출력한다() {
        assert_eq!(format_output(Some(&json!(5))), "5");
        assert_eq!(format_output(Some(&json!(1.0))), "1");
        assert_eq!(format_output(Some(&json!(true))), "true");
        assert_eq!(format_output(Some(&json!(false))), "false");
    }

    #[test]
    fn json_모양이_아닌_문자열은_그대로_돌려준다() {
        assert_eq!(format_output(Some(&json!("plain text"))), "plain text");
        assert_eq!(format_output(Some(&json!(""))), "");
    }

    #[test]
    fn 빈_content_배열은_텍스트로_보지_않는다() {
        assert_eq!(format_output(Some(&json!([]))), "[]");
        assert_eq!(
            format_output(Some(&json!({ "content": [] }))),
            "{\n  \"content\": []\n}"
        );
    }

    #[test]
    fn 텍스트가_아닌_블록이_하나라도_있으면_전체를_json으로_출력한다() {
        for out in [
            json!([{ "type": "text", "text": "a" }, { "type": "image", "data": "x" }]),
            json!([{ "type": "text", "text": 5 }]),
            json!([{ "type": "text" }]),
            json!([{ "text": "no type" }]),
            json!([{ "type": "text", "text": "a" }, null]),
            json!([{ "type": "text", "text": "a" }, "b"]),
            json!([[{ "type": "text", "text": "a" }]]),
            json!([1, 2]),
        ] {
            assert_eq!(format_output(Some(&out)), stringify_pretty(&out), "{out}");
        }
        assert_eq!(format_output(Some(&json!([1, 2]))), "[\n  1,\n  2\n]");
    }

    #[test]
    fn content가_배열이_아니면_객체_전체를_json으로_출력한다() {
        assert_eq!(
            format_output(Some(&json!({ "content": "text" }))),
            "{\n  \"content\": \"text\"\n}"
        );
        assert_eq!(
            format_output(Some(&json!({ "content": { "type": "text", "text": "a" } }))),
            "{\n  \"content\": {\n    \"type\": \"text\",\n    \"text\": \"a\"\n  }\n}"
        );
    }

    #[test]
    fn 텍스트_블록의_다른_필드는_무시하고_객체의_다른_키도_버린다() {
        let out = json!({
            "isError": false,
            "content": [
                { "type": "text", "text": "[1]", "annotations": {} },
                { "type": "text", "text": "" }
            ]
        });
        assert_eq!(format_output(Some(&out)), "[\n  1\n]\n\n");
    }
}
