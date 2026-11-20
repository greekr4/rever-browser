//! JavaScript semantics the tools' output depends on: UTF-16 lengths and
//! slices, `Date.prototype.toISOString`, and object key order.

/// JavaScript's `string.length`.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The longest prefix that fits in `units` UTF-16 code units: what JavaScript's
/// `slice(0, units)` keeps. A surrogate pair split by the cut is left out
/// whole, since a Rust string can't hold the lone half.
pub(crate) fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut seen = 0;
    let cut = text
        .char_indices()
        .find(|(_, c)| {
            seen += c.len_utf16();
            seen > units
        })
        .map_or(text.len(), |(at, _)| at);
    text.get(..cut).unwrap_or(text)
}

pub(crate) use rever_core::time::to_iso_string;

/// The text of the RangeError `toISOString` throws.
pub(crate) const INVALID_TIME: &str = "Invalid time value";

/// The other shortest digit string, when `f` lies exactly halfway between it
/// and `digits`: Rust then rounds up, ECMAScript takes the even one
/// (245010723912258.125 prints as ...258.12 in JavaScript, ...258.13 in Rust).
fn even_on_tie(f: f64, digits: &str, exp: i64) -> Option<String> {
    // Two decimals of 15 digits or fewer never share a double, so there is no
    // tie to break below that; the exact expansion is only worth printing above.
    if digits.len() < 15 {
        return None;
    }
    let (head, last) = digits.split_at_checked(digits.len() - 1)?;
    let last = last.parse::<u32>().ok().filter(|last| last % 2 == 1)?;
    let lower = format!("{head}{}", last - 1);
    // A double has at most 767 significant digits: this is its exact value.
    let exact = format!("{f:.800e}");
    let exact = exact.split_once('e')?.0.replace('.', "");
    let rest = exact.strip_prefix(&lower)?.strip_prefix('5')?;
    let tie = rest.bytes().all(|digit| digit == b'0');
    // Just above a power of two the gap below is half as wide, and the lower
    // string may read back as the next double down.
    let scale = exp - i64::try_from(lower.len()).ok()? + 1;
    let same = format!("{lower}e{scale}").parse::<f64>().ok() == Some(f);
    (tie && same).then_some(lower)
}

/// `Number.prototype.toString()`: the shortest digits that round-trip, written
/// out in full from 1e-7 up to 1e21 and in exponent notation outside that.
fn js_number(f: f64) -> String {
    if f == 0.0 {
        return "0".to_owned();
    }
    if !f.is_finite() {
        // What JSON.stringify writes for NaN and the infinities.
        return "null".to_owned();
    }
    // `{:e}` gives those same shortest digits, as "d.ddde±x".
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp = exp.parse::<i64>().unwrap_or(0);
    let digits = mantissa.replace('.', "");
    let digits = even_on_tie(f.abs(), &digits, exp).unwrap_or(digits);
    // Position of the decimal point relative to the first digit.
    let point = exp + 1;
    let count = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    let zeros = |n: i64| "0".repeat(usize::try_from(n).unwrap_or(0));
    let text = if (count..=21).contains(&point) {
        format!("{digits}{}", zeros(point - count))
    } else if (1..=21).contains(&point) {
        let (int, frac) = digits
            .split_at_checked(usize::try_from(point).unwrap_or(0))
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

/// `JSON.stringify(value)`: serde_json's compact form, except that a number is
/// written as the double JavaScript holds. serde_json prints `1.0`, `1e16` and
/// `1e-6` where JavaScript prints `1`, `10000000000000000` and `0.000001`.
pub(crate) fn json_stringify(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), js_number),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(json_stringify).collect();
            format!("[{}]", items.join(","))
        }
        Value::Object(map) => {
            let members: Vec<String> = map
                .iter()
                .map(|(key, item)| {
                    format!("{}:{}", Value::from(key.as_str()), json_stringify(item))
                })
                .collect();
            format!("{{{}}}", members.join(","))
        }
        other => other.to_string(),
    }
}

/// An "array index" property name: JavaScript lists these first, in numeric order.
fn array_index(key: &str) -> Option<u32> {
    let n: u32 = key.parse().ok()?;
    (n != u32::MAX && n.to_string() == key).then_some(n)
}

/// Reorders object members the way JavaScript enumerates them: array indices
/// ascending, then the rest in insertion order.
pub(crate) fn js_key_order<V>(members: impl IntoIterator<Item = (String, V)>) -> Vec<(String, V)> {
    let (mut indices, rest): (Vec<_>, Vec<_>) = members
        .into_iter()
        .partition(|(key, _)| array_index(key).is_some());
    indices.sort_by_key(|(key, _)| array_index(key));
    indices.into_iter().chain(rest).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 길이와_자르기는_utf16_단위로_센다() {
        assert_eq!(utf16_len("가😀a"), 4);
        assert_eq!(utf16_prefix("가😀a", 1), "가");
        // The cut lands inside the emoji's surrogate pair: it is left out whole.
        assert_eq!(utf16_prefix("가😀a", 2), "가");
        assert_eq!(utf16_prefix("가😀a", 3), "가😀");
        assert_eq!(utf16_prefix("가😀a", 99), "가😀a");
        assert_eq!(utf16_prefix("abc", 0), "");
    }

    #[test]
    fn 밀리초를_to_iso_string과_같은_문자열로_쓴다() {
        // Expected strings are what `new Date(ms).toISOString()` prints in node.
        let cases = [
            (0, "1970-01-01T00:00:00.000Z"),
            (1_700_000_000_123, "2023-11-14T22:13:20.123Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            (253_402_300_799_999, "9999-12-31T23:59:59.999Z"),
            (253_402_300_800_000, "+010000-01-01T00:00:00.000Z"),
            (8_640_000_000_000_000, "+275760-09-13T00:00:00.000Z"),
        ];
        for (ms, expected) in cases {
            assert_eq!(to_iso_string(ms).as_deref(), Some(expected), "{ms}");
        }
        assert_eq!(to_iso_string(8_640_000_000_000_001), None);
        assert_eq!(to_iso_string(u64::MAX), None);
    }

    #[test]
    fn 숫자를_자바스크립트가_쓰는_대로_쓴다() {
        // Expected strings are what `String(n)` prints in node.
        let cases = [
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (1.5, "1.5"),
            (-2.5, "-2.5"),
            (0.1, "0.1"),
            (4.35, "4.35"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (1e16, "10000000000000000"),
            (9_007_199_254_740_993.0, "9007199254740992"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (0.000015, "0.000015"),
            (123_456_789_012_345_680_000.0, "123456789012345680000"),
            (1.234_567_890_123_456_8e29, "1.2345678901234568e+29"),
            (5e-324, "5e-324"),
            (18_446_744_073_709_551_616.0, "18446744073709552000"),
            (1e300, "1e+300"),
            (123_456.789e3, "123456789"),
            // Halfway between two shortest strings: JavaScript takes the even one.
            (245_010_723_912_258.13, "245010723912258.12"),
            (967_273_786_048_676.3, "967273786048676.2"),
            (-885_696_802_133.781_3, "-885696802133.7812"),
            // Halfway, and the upper string is the even one already.
            (967_273_786_048_676.8, "967273786048676.8"),
            (f64::NAN, "null"),
            (f64::INFINITY, "null"),
        ];
        for (n, expected) in cases {
            assert_eq!(js_number(n), expected, "{n:e}");
        }
    }

    #[test]
    fn json_stringify는_숫자만_빼고_serde_json의_압축_출력과_같다() {
        use serde_json::json;
        let value = json!({
            "s": "a\"b\n한😀\u{1}", "t": true, "n": null, "e": [], "o": {},
            "list": [1, 2.0, 1e16, 1e-6, -0.5, u64::MAX], "z": { "k": 1e21 }
        });
        // What JSON.stringify prints in node for the same value.
        assert_eq!(
            json_stringify(&value),
            r#"{"s":"a\"b\n한😀\u0001","t":true,"n":null,"e":[],"o":{},"list":[1,2,10000000000000000,0.000001,-0.5,18446744073709552000],"z":{"k":1e+21}}"#
        );
        assert_eq!(json_stringify(&json!("x")), "\"x\"");
        assert_eq!(json_stringify(&json!(42)), "42");
        assert_eq!(json_stringify(&json!(null)), "null");
    }

    #[test]
    fn 키는_자바스크립트처럼_정수_키를_먼저_오름차순으로_둔다() {
        let members = ["b", "10", "a", "2", "01", "4294967295"].map(|k| (k.to_owned(), ()));
        let keys: Vec<String> = js_key_order(members).into_iter().map(|(k, _)| k).collect();
        // "01" and 2^32-1 are not array indices, so they keep their place.
        assert_eq!(keys, ["2", "10", "b", "a", "01", "4294967295"]);
    }
}
