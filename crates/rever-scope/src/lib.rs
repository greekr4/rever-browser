//! Red Team scope, fence and findings logic, ported from src/main/mcp

use std::sync::{Mutex, MutexGuard, PoisonError};

use regex::Regex;

pub mod fence;
pub mod findings_dedupe;
pub mod findings_scope;
pub mod ip_guard;
pub mod redteam_tenants;
pub mod scope;

/// JavaScript's whitespace set (`\s`, `String.prototype.trim`). Rust's differs
/// by two characters: U+FEFF is whitespace only in JS, U+0085 only in Rust.
const JS_WHITESPACE_CLASS: &str = r"[\t\n\x0B\x0C\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";

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

pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// Compiles a pattern whose `\s`, `\w`, `\d` and `\b` keep their JavaScript
/// (non-`u` flag) meaning: ASCII-only word characters and digits. Left to the
/// regex crate they are Unicode-aware, so `\b` would stop seeing a boundary
/// between Hangul and an ASCII word, which the finding titles rely on.
///
/// Only for the fixed patterns in this crate — the translation is textual.
pub(crate) fn js_regex(pattern: &str) -> Regex {
    let translated = pattern
        .replace(r"\s", JS_WHITESPACE_CLASS)
        .replace(r"\w", "[0-9A-Za-z_]")
        .replace(r"\d", "[0-9]")
        .replace(r"\b", r"(?-u:\b)");
    Regex::new(&translated).expect("built-in pattern compiles")
}

/// The module state mirrors the TypeScript module-level variables, so a panic
/// elsewhere must not make every later scope/fence call panic too.
pub(crate) fn lock<T>(mutex: &'static Mutex<T>) -> MutexGuard<'static, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Test stand-in for vitest's per-file isolation plus `afterEach`: the state is
/// process-wide and cargo runs tests on parallel threads, so tests touching it
/// run one at a time and reset it on entry and exit.
#[cfg(test)]
pub(crate) struct Isolated {
    reset: fn(),
    _serial: MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for Isolated {
    fn drop(&mut self) {
        (self.reset)();
    }
}

#[cfg(test)]
pub(crate) fn isolated(reset: fn()) -> Isolated {
    static SERIAL: Mutex<()> = Mutex::new(());
    let serial = lock(&SERIAL);
    reset();
    Isolated {
        reset,
        _serial: serial,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_공백과_단어_경계는_자바스크립트_정의를_따른다() {
        assert_eq!(js_trim("\u{FEFF} a\u{85} "), "a\u{85}");
        // ASCII-only \b and \w: "é" is not a word character in JavaScript.
        assert!(js_regex(r"\bcaf\b").is_match("café"));
        assert!(!js_regex(r"^\w+$").is_match("café"));
        assert!(!js_regex(r"^\d$").is_match("٣"));
        assert!(js_regex(r"^[^\s]\s$").is_match("a\u{FEFF}"));
    }
}
