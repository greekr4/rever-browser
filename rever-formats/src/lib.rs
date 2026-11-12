//! Decoders and matchers for captured data (protobuf, console, intercept rules, traces), ported from src/main

pub mod console_format;
pub mod exec_trace;
pub mod intercept_match;
pub mod listeners;
pub mod protobuf_decode;
pub mod script_registry;

/// What JavaScript's `\s` and `String.prototype.trim` call whitespace. Rust's
/// `char::is_whitespace` differs on U+0085 (not JS whitespace) and U+FEFF (is).
pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}
