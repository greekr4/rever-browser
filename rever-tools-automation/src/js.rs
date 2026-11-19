//! JavaScript semantics the ported code depends on: whitespace, UTF-16 lengths,
//! truthiness, and how a number or a JSON value is printed.

use std::hash::{BuildHasher, Hasher};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde_json::Value;

/// JavaScript's `\s` as a `regex` class: Rust's differs by U+FEFF (JS only) and
/// U+0085 (Rust only).
pub(crate) const JS_SPACE: &str = r"[\s\x{feff}--\x{85}]";

/// JavaScript's `\S`.
const JS_NOT_SPACE: &str = r"[[^\s\x{feff}]\x{85}]";

/// `{n}`, `{n,}` or `{n,m}` just past its `{`; any other brace is a literal in JavaScript.
static QUANTIFIER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]+(,[0-9]*)?\}").expect("built-in pattern compiles"));

/// `new RegExp(source)`, without flags, on the `regex` crate. The two syntaxes
/// look alike but disagree where a pattern meets non-ASCII text: `\d`, `\w` and
/// `\b` are ASCII in JavaScript, `\s` is its own whitespace set, `.` stops at
/// every line terminator, a brace that is not a quantifier is a literal, and a
/// class has no set operators. `None` is a pattern JavaScript rejects, or one
/// this engine cannot run.
// shortcut: lookaround and backreferences have no translation here (the pattern
// is then `None`), `\c` control escapes are not read, a doubled quantifier
// (`a**`) is accepted, and an astral character is one character where
// JavaScript sees two units. Move to a backtracking engine if patterns come to
// need any of these.
pub(crate) fn js_regex(source: &str) -> Option<Regex> {
    // Writes `c` so that it can only be read as itself, in or out of a class.
    fn literal(out: &mut String, c: char) {
        if c.is_alphanumeric() {
            out.push(c);
        } else {
            out.push_str(&format!("\\x{{{:x}}}", u32::from(c)));
        }
    }
    let mut out = String::new();
    let mut chars = source.chars();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        match (c, in_class) {
            ('\\', _) => match (chars.next()?, in_class) {
                ('d', _) => out.push_str("[0-9]"),
                ('D', _) => out.push_str("[^0-9]"),
                ('w', _) => out.push_str("[0-9A-Za-z_]"),
                ('W', _) => out.push_str("[^0-9A-Za-z_]"),
                ('s', _) => out.push_str(JS_SPACE),
                ('S', _) => out.push_str(JS_NOT_SPACE),
                ('b', false) => out.push_str(r"(?-u:\b)"),
                ('B', false) => out.push_str(r"(?-u:\B)"),
                // Inside a class `\b` is a backspace.
                ('b', true) => out.push_str(r"\x08"),
                ('0', _) => out.push_str(r"\x00"),
                ('1'..='9', false) => return None,
                ('k', false) if source.contains("(?<") => return None,
                // Read alike by both engines.
                (same @ ('t' | 'n' | 'r' | 'f' | 'v'), _) => {
                    out.push('\\');
                    out.push(same);
                }
                // `\x41` and `\u0041` too, but short of its digits the letter is itself.
                (hex @ ('x' | 'u'), _) => {
                    let digits = if hex == 'x' { 2 } else { 4 };
                    let ahead = chars.as_str().bytes().take(digits);
                    if ahead.filter(u8::is_ascii_hexdigit).count() == digits {
                        out.push('\\');
                    }
                    out.push(hex);
                }
                // Anything else escapes to itself (`\/`, `\-`, `\A`, `\p`), where
                // the `regex` crate may read an assertion or a Unicode class.
                (other, _) => literal(&mut out, other),
            },
            ('[', false) => {
                let negated = chars.as_str().starts_with('^');
                if negated {
                    chars.next();
                }
                if chars.as_str().starts_with(']') {
                    chars.next();
                    // `[]` matches nothing and `[^]` anything; the `regex` crate
                    // would read the `]` as a member.
                    out.push_str(if negated { r"[\s\S]" } else { r"[^\s\S]" });
                } else {
                    out.push_str(if negated { "[^" } else { "[" });
                    in_class = true;
                }
            }
            (']', true) => {
                out.push(']');
                in_class = false;
            }
            // A second `-` in a row would be the `regex` crate's set difference.
            ('-', true) if !out.ends_with('-') => out.push('-'),
            (member, true) => literal(&mut out, member),
            ('.', false) => out.push_str(r"[^\n\r\x{2028}\x{2029}]"),
            ('{', false) => match QUANTIFIER.find(chars.as_str()) {
                Some(found) => {
                    out.push('{');
                    out.push_str(found.as_str());
                    chars = chars.as_str().get(found.end()..)?.chars();
                }
                None => literal(&mut out, '{'),
            },
            ('}' | ']', false) => literal(&mut out, c),
            ('(', false) => {
                if let Some(group) = chars.as_str().strip_prefix('?') {
                    let named = group.starts_with('<')
                        && !group.starts_with("<=")
                        && !group.starts_with("<!");
                    let flagged = group
                        .trim_start_matches(['i', 'm', 's', '-'])
                        .starts_with(':');
                    // Lookaround cannot run here, and `(?i)` is not JavaScript.
                    if !named && !flagged {
                        return None;
                    }
                }
                out.push('(');
            }
            (other, false) => out.push(other),
        }
    }
    if in_class {
        return None;
    }
    Regex::new(&out).ok()
}

/// A property name that is an array index: "1" is, "01" and "-1" are not.
pub(crate) fn array_index(key: &str) -> Option<usize> {
    key.parse::<usize>()
        .ok()
        .filter(|index| index.to_string() == key)
}

/// What JavaScript's `\s` and `trim()` call whitespace.
pub(crate) fn is_js_space(c: char) -> bool {
    c == '\u{feff}' || (c.is_whitespace() && c != '\u{85}')
}

pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// `s.slice(0, units)`. A cut through a surrogate pair leaves that character out,
/// since a Rust string cannot hold half of it.
pub(crate) fn utf16_prefix(s: &str, units: usize) -> &str {
    let mut seen = 0;
    for (byte, c) in s.char_indices() {
        seen += c.len_utf16();
        if seen > units {
            return s.get(..byte).unwrap_or(s);
        }
    }
    s
}

/// `String(number)`: no trailing ".0", plain digits up to 1e21 and down to 1e-6,
/// and a signed exponent beyond that.
pub(crate) fn js_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    if !value.is_finite() {
        // JSON cannot carry these; JSON.stringify prints null for them.
        return "null".to_owned();
    }
    // Rust's exponent form has the shortest digits that round-trip, like JavaScript's.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let point = exponent.parse::<i32>().unwrap_or(0) + 1;
    let count = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    let zeros = |n: i32| "0".repeat(usize::try_from(n).unwrap_or(0));
    let split = |at: i32| digits.split_at_checked(usize::try_from(at).unwrap_or(0));
    let body = if count <= point && point <= 21 {
        format!("{digits}{}", zeros(point - count))
    } else if 0 < point && point <= 21 {
        let (whole, fraction) = split(point).unwrap_or((&digits, ""));
        format!("{whole}.{fraction}")
    } else if -6 < point && point <= 0 {
        format!("0.{}{digits}", zeros(-point))
    } else {
        let (first, rest) = split(1).unwrap_or((&digits, ""));
        let dot = if rest.is_empty() { "" } else { "." };
        let sign = if point > 0 { '+' } else { '-' };
        format!("{first}{dot}{rest}e{sign}{}", (point - 1).abs())
    };
    if value < 0.0 {
        format!("-{body}")
    } else {
        body
    }
}

/// `JSON.stringify(value)`, or `JSON.stringify(value, null, 2)` when `pretty`.
/// serde_json alone prints 5.0 where JavaScript prints 5.
pub(crate) fn json_stringify(value: &Value, pretty: bool) -> String {
    let mut out = String::new();
    write_json(&mut out, value, pretty.then_some(0));
    out
}

fn write_json(out: &mut String, value: &Value, depth: Option<usize>) {
    let inner = depth.map(|depth| depth + 1);
    let line = |out: &mut String, depth: Option<usize>| {
        if let Some(depth) = depth {
            out.push('\n');
            out.push_str(&"  ".repeat(depth));
        }
    };
    match value {
        Value::Number(number) if number.is_f64() => {
            out.push_str(&js_number(number.as_f64().unwrap_or(0.0)));
        }
        Value::Array(items) if !items.is_empty() => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                line(out, inner);
                write_json(out, item, inner);
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
                line(out, inner);
                out.push_str(&Value::from(key.as_str()).to_string());
                out.push_str(if depth.is_some() { ": " } else { ":" });
                write_json(out, item, inner);
            }
            line(out, depth);
            out.push('}');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// `String(value)`.
pub(crate) fn js_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) if number.is_f64() => js_number(number.as_f64().unwrap_or(0.0)),
        Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|item| {
                    if item.is_null() {
                        String::new()
                    } else {
                        js_string(item)
                    }
                })
                .collect();
            parts.join(",")
        }
        Value::Object(_) => "[object Object]".to_owned(),
        other => other.to_string(),
    }
}

/// `String(payload[key])`, where a missing key is `undefined`.
pub(crate) fn js_string_at(payload: &Value, key: &str) -> String {
    payload
        .get(key)
        .map_or_else(|| "undefined".to_owned(), js_string)
}

/// Whether `if (value)` runs.
pub(crate) fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `Date.now()`.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn base36(mut number: u64) -> String {
    let mut digits = Vec::new();
    loop {
        // A remainder of 36 is always a base-36 digit.
        digits.push(char::from_digit((number % 36) as u32, 36).unwrap_or('0'));
        number /= 36;
        if number == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}

/// `${prefix}${Date.now().toString(36)}${Math.random().toString(36).slice(2, 2 + random)}`,
/// the shape of workflow and step ids.
pub(crate) fn new_id(prefix: char, random: usize) -> String {
    // A fresh RandomState is seeded by the OS: enough to tell apart ids made in
    // the same millisecond, which is all Math.random() did here.
    let noise = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let tail = format!("{:0>13}", base36(noise));
    format!(
        "{prefix}{}{}",
        base36(now_ms()),
        tail.get(..random).unwrap_or(&tail)
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn 숫자는_자바스크립트처럼_출력한다() {
        // Expected strings are what String(n) gives in bun.
        assert_eq!(js_number(5.0), "5");
        assert_eq!(js_number(-0.0), "0");
        assert_eq!(js_number(1.5), "1.5");
        assert_eq!(js_number(-123.456), "-123.456");
        assert_eq!(js_number(0.000001), "0.000001");
        assert_eq!(js_number(0.0000001), "1e-7");
        assert_eq!(js_number(1.5e-7), "1.5e-7");
        assert_eq!(js_number(1e21), "1e+21");
        assert_eq!(js_number(1.25e21), "1.25e+21");
        assert_eq!(js_number(123456789012345680000.0), "123456789012345680000");
        assert_eq!(js_number(10000.0), "10000");
        assert_eq!(js_number(0.1), "0.1");
    }

    #[test]
    fn json은_json_stringify와_같은_모양으로_출력한다() {
        // Expected strings are what JSON.stringify prints in bun.
        let value = json!({ "a": [1, 2.0, { "b": "한\"\n" }], "e": [], "o": {}, "n": null });
        assert_eq!(
            json_stringify(&value, false),
            r#"{"a":[1,2,{"b":"한\"\n"}],"e":[],"o":{},"n":null}"#
        );
        assert_eq!(
            json_stringify(&value, true),
            "{\n  \"a\": [\n    1,\n    2,\n    {\n      \"b\": \"한\\\"\\n\"\n    }\n  ],\n  \"e\": [],\n  \"o\": {},\n  \"n\": null\n}"
        );
        assert_eq!(json_stringify(&json!([]), true), "[]");
        assert_eq!(json_stringify(&json!("x"), true), "\"x\"");
    }

    #[test]
    fn 문자열_변환과_참거짓은_자바스크립트_규칙을_따른다() {
        assert_eq!(js_string(&json!("a")), "a");
        assert_eq!(js_string(&json!(5.0)), "5");
        assert_eq!(js_string(&json!(true)), "true");
        assert_eq!(js_string(&json!([1, null, "b", [2, 3]])), "1,,b,2,3");
        assert_eq!(js_string(&json!({ "a": 1 })), "[object Object]");
        assert_eq!(js_string_at(&json!({}), "id"), "undefined");
        assert_eq!(js_string_at(&json!({ "id": null }), "id"), "null");
        assert!(!js_truthy(&json!(0)) && !js_truthy(&json!("")) && !js_truthy(&Value::Null));
        assert!(js_truthy(&json!([])) && js_truthy(&json!({})) && js_truthy(&json!("0")));
    }

    #[test]
    fn 공백과_길이는_자바스크립트_기준이다() {
        assert_eq!(js_trim("\u{feff} x\u{a0}\u{2028}"), "x");
        assert_eq!(js_trim("\u{85}x"), "\u{85}x");
        assert_eq!(utf16_prefix("가나다", 2), "가나");
        assert_eq!(utf16_prefix("a😀b", 2), "a");
        assert_eq!(utf16_prefix("a😀b", 3), "a😀");
        assert_eq!(utf16_prefix("ab", 9), "ab");
    }

    #[test]
    fn 정규식은_자바스크립트가_읽는_대로_읽는다() {
        // Each row is what `new RegExp(pattern).test(text)` answers under bun
        // (false too when the constructor throws).
        let cases: &[(&str, &str, bool)] = &[
            // ASCII-only \w \d \b, and JavaScript's own whitespace set.
            (r"^\w+$", "성공", false),
            (r"\bok\b", "한ok한", true),
            (r"^\d$", "٣", false),
            (r"^\s$", "\u{feff}", true),
            (r"^\s$", "\u{85}", false),
            (r"[\w-]+@x", "한글-a@x", true),
            (r"^[^\d]$", "٣", true),
            (r"^\D$", "٣", true),
            (r"^\W$", "한", true),
            (r"^\S$", "\u{85}", true),
            (r"\Bk", "ok", true),
            // `.` stops at every line terminator.
            ("a.b", "a\rb", false),
            ("a.b", "a\nb", false),
            ("a.b", "a\u{2028}b", false),
            (r"^[\s\S]*$", "a\nb", true),
            // A brace or bracket that opens nothing is itself.
            ("a{", "a{", true),
            ("a{1", "a{1", true),
            ("x]", "x]", true),
            ("^a{2,3}$", "aaa", true),
            ("^a{2}$", "aaa", false),
            // Escapes.
            (r"\/api\/v1", "/api/v1", true),
            (r"[\b]", "\u{8}", true),
            (r"\u0041", "A", true),
            (r"\x41", "A", true),
            (r"\x4", "x4", true),
            (r"\u12", "u12", true),
            (r"[\u0041-\u005a]", "Q", true),
            (r"\t[\t]\v\f", "\t\t\u{b}\u{c}", true),
            (r"\p{L}", "p{L}", true),
            (r"\e", "e", true),
            (r"\-", "-", true),
            (r"\0", "\0", true),
            (r"\k<n>", "k<n>", true),
            (r"\<", "<", true),
            (r"\A", "A", true),
            (r"\z", "z", true),
            // A class has members only: no nesting, no set operators.
            ("[^]", "x", true),
            ("[]", "x", false),
            ("[[]", "[", true),
            ("[a&&b]", "&", true),
            ("[a--b]", "-", false),
            ("[a~~b]", "~", true),
            ("[a-z&&[^b]]", "b]", true),
            ("[[:alpha:]]", "a]", true),
            (r"[\]][\\]", r"]\", true),
            (r"[a\-z][-a][a-][\d-]", "----", true),
            ("[.]", "x", false),
            (r"[^\W]", "한", false),
            (r"^[\w.+-]+@[\w-]+\.[\w.]+$", "a.b+c@x-y.co.kr", true),
            ("^[가-힣]+$", "성공", true),
            (r"\bcafé\b", "café", false),
            ("a{,2}", "a{,2}", true),
            ("a{2,1}", "aa", false),
            // Groups.
            ("(?:a|b)+?c", "abc", true),
            ("(?<n>a)", "a", true),
            ("(?i)ok", "OK", false),
            // Plain text and what JavaScript rejects.
            ("^$", "", true),
            ("#", "#", true),
            ("a b", "a b", true),
            (r#""status":\s*200"#, r#"{"status": 200}"#, true),
            ("a\\", "a", false),
            ("[", "[", false),
            ("(", "(", false),
        ];
        for (pattern, text, expected) in cases {
            let matched = js_regex(pattern).is_some_and(|regex| regex.is_match(text));
            assert_eq!(matched, *expected, "/{pattern}/ on {text:?}");
        }
        // Where JavaScript matches and this engine cannot: no pattern at all,
        // rather than one that means something else.
        assert!(js_regex("(?<=a)b").is_none());
        assert!(js_regex("(?!a)b").is_none());
        assert!(js_regex(r"(a)\1").is_none());
        assert!(js_regex(r"(?<n>a)\k<n>").is_none());
    }

    #[test]
    fn 배열_인덱스는_정규형_숫자_이름뿐이다() {
        assert_eq!(array_index("0"), Some(0));
        assert_eq!(array_index("12"), Some(12));
        for not_index in ["01", "+1", "-1", "1.0", "", " 1", "1e0"] {
            assert_eq!(array_index(not_index), None, "{not_index}");
        }
    }

    #[test]
    fn 새_id는_접두어와_시각과_무작위_꼬리로_이뤄진다() {
        let id = new_id('w', 6);
        assert!(id.starts_with('w'));
        assert!(id.len() > 7);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
        assert_ne!(new_id('s', 4), new_id('s', 4));
    }
}
