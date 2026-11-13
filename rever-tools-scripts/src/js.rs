//! JavaScript string semantics the ported tools depend on: lengths and slices
//! count UTF-16 code units, whitespace is JavaScript's set, and base64 is
//! decoded as leniently as Node's `Buffer.from(text, 'base64')`.

use base64::Engine as _;
use base64::alphabet::STANDARD;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

/// JavaScript's `\s` as a `regex` class: Rust's differs by U+FEFF (JS only) and
/// U+0085 (Rust only).
pub(crate) const JS_SPACE: &str = r"[\s\x{feff}--\x{85}]";
/// JavaScript's `\S`.
pub(crate) const JS_NOT_SPACE: &str = r"[^\s\x{feff}--\x{85}]";

/// What JavaScript's `\s` and `trim()` call whitespace.
pub(crate) fn is_js_space(c: char) -> bool {
    c == '\u{feff}' || (c.is_whitespace() && c != '\u{85}')
}

pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// JavaScript's `string.length`.
pub(crate) fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// JavaScript's `s.slice(start, end)` for non-negative offsets. A cut through
/// the middle of a surrogate pair would leave half a character, which a Rust
/// string cannot hold: that character is left out.
pub(crate) fn utf16_slice(s: &str, start: usize, end: usize) -> &str {
    let mut units = 0;
    let mut from = None;
    let mut to = s.len();
    for (byte, c) in s.char_indices() {
        if from.is_none() && units >= start {
            from = Some(byte);
        }
        units += c.len_utf16();
        if units > end {
            to = byte;
            break;
        }
    }
    s.get(from.unwrap_or(s.len())..to).unwrap_or("")
}

const LENIENT: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// `Buffer.from(text, 'base64')`: Node never fails. It stops at the first `=`,
/// skips every character outside the alphabet (the URL-safe `-` and `_` count
/// as `+` and `/`), and drops a trailing symbol that cannot complete a byte.
pub(crate) fn node_base64_decode(text: &str) -> Vec<u8> {
    let mut symbols: Vec<u8> = text
        .bytes()
        .take_while(|b| *b != b'=')
        .filter_map(|b| match b {
            b'-' => Some(b'+'),
            b'_' => Some(b'/'),
            b'+' | b'/' => Some(b),
            _ => b.is_ascii_alphanumeric().then_some(b),
        })
        .collect();
    if symbols.len() % 4 == 1 {
        symbols.pop();
    }
    LENIENT.decode(&symbols).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 길이와_자르기는_utf16_단위로_센다() {
        // "😀" is two UTF-16 units and four bytes; "가" is one unit and three bytes.
        assert_eq!(utf16_len("a😀가"), 4);
        assert_eq!(utf16_slice("abcdef", 1, 4), "bcd");
        assert_eq!(utf16_slice("가나다라", 1, 3), "나다");
        assert_eq!(utf16_slice("abc", 2, 99), "c");
        assert_eq!(utf16_slice("abc", 5, 9), "");
        assert_eq!(utf16_slice("abc", 2, 1), "");
        assert_eq!(utf16_slice("", 0, 4), "");
    }

    #[test]
    fn 서로게이트_쌍_한가운데를_자르면_그_글자는_빠진다() {
        assert_eq!(utf16_slice("a😀b", 0, 2), "a");
        assert_eq!(utf16_slice("a😀b", 2, 4), "b");
        assert_eq!(utf16_slice("a😀b", 1, 3), "😀");
    }

    #[test]
    fn 공백은_자바스크립트_기준이다() {
        assert_eq!(js_trim("\u{feff} x\u{a0}\u{2028}"), "x");
        assert_eq!(js_trim("\u{85}x"), "\u{85}x");
    }

    #[test]
    fn base64는_node처럼_너그럽게_푼다() {
        // Expected values are what Buffer.from(text, 'base64') gives under node.
        assert_eq!(node_base64_decode("YWJj"), b"abc");
        assert_eq!(node_base64_decode("YQ=="), b"a");
        assert_eq!(node_base64_decode("YQ==YQ=="), b"a");
        assert_eq!(node_base64_decode("YW Jj\nYWJj"), b"abcabc");
        assert_eq!(node_base64_decode("YWJ*jYQ"), b"abca");
        assert_eq!(node_base64_decode("YWJjY"), b"abc");
        assert_eq!(node_base64_decode("YWJjYW"), b"abca");
        assert_eq!(node_base64_decode("-_-_"), [0xfb, 0xff, 0xbf]);
        assert_eq!(node_base64_decode("=YQ"), b"");
        assert_eq!(node_base64_decode("한글"), b"");
    }
}
