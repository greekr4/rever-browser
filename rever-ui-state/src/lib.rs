//! Interface state stores (tabs, settings, drafts) ported from src/renderer/src/stores, without GPUI

pub mod agent_choice;
pub mod agent_mode;
pub mod app_theme;
pub mod approval_audit;
pub mod browser_mode;
pub mod chat_collapsed;
pub mod chat_draft;
pub mod chat_history;
pub mod constants;
pub mod format_json;
pub mod ip_visibility;
pub mod navigation_request;
pub mod notify;
pub mod scan_mode;
pub mod storage;
pub mod tabs;
pub mod viewport;
pub mod webview_theme;
pub mod ws_sequence;

pub use storage::{FileStorage, MemoryStorage, Storage};

/// JavaScript's whitespace set (`String.prototype.trim`). Rust's differs by two
/// characters: U+FEFF is whitespace only in JS, U+0085 only in Rust.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `text.slice(0, units)` when the text is longer than `units` UTF-16 code
/// units, `None` when it already fits. A surrogate pair split by the cut is
/// left out whole, since a Rust string can't hold the lone half.
pub(crate) fn utf16_cut(text: &str, units: usize) -> Option<&str> {
    let mut seen = 0;
    let (at, _) = text.char_indices().find(|(_, c)| {
        seen += c.len_utf16();
        seen > units
    })?;
    text.get(..at)
}

/// `n.toString(36)` for a non-negative integer.
pub(crate) fn to_base36(mut n: u64) -> String {
    let mut digits = Vec::new();
    loop {
        // n % 36 is always a valid base-36 digit.
        digits.extend(char::from_digit((n % 36) as u32, 36));
        n /= 36;
        if n == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}

/// `random.toString(36).slice(2, 8)` for a `Math.random()` value in `[0, 1)`:
/// the id suffix the stores append. Follows V8's radix conversion (digits only
/// up to the double's precision, round-to-even on the last one) so the same
/// number gives the same suffix. Anything outside `[0, 1)` gives no digits.
pub(crate) fn random_suffix(random: f64) -> String {
    if !(0.0..1.0).contains(&random) {
        return String::new();
    }
    let mut fraction = random;
    let mut delta = (0.5 * (random.next_up() - random)).max(f64::from_bits(1));
    let mut digits: Vec<u32> = Vec::new();
    while fraction >= delta {
        fraction *= 36.0;
        delta *= 36.0;
        let digit = fraction as u32;
        digits.push(digit);
        fraction -= f64::from(digit);
        let round_up = fraction > 0.5 || (fraction == 0.5 && digit & 1 == 1);
        if round_up && fraction + delta > 1.0 {
            // Carry into the digits already written; running out of them means
            // the value rounded up to "1", which has nothing after the point.
            while let Some(last) = digits.pop() {
                if last + 1 < 36 {
                    digits.push(last + 1);
                    break;
                }
            }
            break;
        }
    }
    digits
        .iter()
        .take(6)
        .filter_map(|&d| char::from_digit(d, 36))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_trim은_자바스크립트_공백만_잘라낸다() {
        assert_eq!(js_trim("\u{FEFF} a \u{A0}\n"), "a");
        // NEL is whitespace for Rust's trim but not for JavaScript's.
        assert_eq!(js_trim("\u{85}a\u{85}"), "\u{85}a\u{85}");
    }

    #[test]
    fn utf16_cut은_utf16_단위로_자르고_짧으면_none이다() {
        assert_eq!(utf16_cut("abc", 3), None);
        assert_eq!(utf16_cut("abcd", 3), Some("abc"));
        assert_eq!(utf16_cut("가나다", 2), Some("가나"));
        // An emoji is two units: a cut through it drops it whole.
        assert_eq!(utf16_cut("a😀b", 2), Some("a"));
        assert_eq!(utf16_cut("a😀b", 3), Some("a😀"));
    }

    #[test]
    fn to_base36은_자바스크립트_tostring_36과_같다() {
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(35), "z");
        assert_eq!(to_base36(36), "10");
        // (1700000000000).toString(36)
        assert_eq!(to_base36(1_700_000_000_000), "loyw3v28");
    }

    #[test]
    fn random_suffix는_소수부_앞_여섯_자리를_돌려준다() {
        // (0.5).toString(36) === "0.i"
        assert_eq!(random_suffix(0.5), "i");
        // (0.123456789).toString(36) === "0.4fzzzxjylrx"
        assert_eq!(random_suffix(0.123456789), "4fzzzx");
        // (0.9999999999999999).toString(36) === "0.zzzzzzzzzza"
        assert_eq!(random_suffix(0.999_999_999_999_999_9), "zzzzzz");
        // (0.1).toString(36) === "0.3lllllllllm"
        assert_eq!(random_suffix(0.1), "3lllll");
        // (1 / 3).toString(36) === "0.c": the digits stop at the double's precision.
        assert_eq!(random_suffix(1.0 / 3.0), "c");
        // (0.000001).toString(36) === "0.0001ogs5wo29m8"
        assert_eq!(random_suffix(0.000001), "0001og");
        assert_eq!(random_suffix(0.0), "");
    }

    #[test]
    fn random_suffix는_범위_밖_값에_빈_문자열을_돌려준다() {
        assert_eq!(random_suffix(f64::NAN), "");
        assert_eq!(random_suffix(1.0), "");
        assert_eq!(random_suffix(-0.25), "");
    }
}
