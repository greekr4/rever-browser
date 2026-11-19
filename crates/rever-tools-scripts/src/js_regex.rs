//! Compiles a JavaScript regular expression — what `new RegExp(source, flags)`
//! reads without the `u` flag — with the `regex` crate.
//!
//! The two syntaxes look alike but disagree on details an agent's pattern hits
//! in practice: `\d`, `\w`, `\s` and `\b` are ASCII (or JavaScript's own
//! whitespace set) in JavaScript, `.` stops at `\r` as well as `\n`, an
//! unmatched `{` is a literal, and a `[` inside a class is a literal. What the
//! `regex` crate cannot express at all (lookaround, backreferences) is rejected
//! with an error shaped like V8's.

use regex::Regex;

use crate::js::{JS_NOT_SPACE, JS_SPACE};

const DIGIT: &str = "0-9";
const WORD: &str = "0-9A-Za-z_";

/// `new RegExp(source, flags)`. `g` and `d` are accepted and change nothing:
/// every search here is global and nothing reads match indices.
pub(crate) fn js_regex(source: &str, flags: &str) -> Result<Regex, String> {
    let invalid = |reason: &str| format!("Invalid regular expression: /{source}/{flags}: {reason}");
    let mut prefix = String::new();
    let mut dot_all = false;
    for (at, flag) in flags.char_indices() {
        let repeated = flags.get(..at).is_some_and(|seen| seen.contains(flag));
        if repeated || !"dgimsuvy".contains(flag) {
            return Err(format!(
                "Invalid flags supplied to RegExp constructor '{flags}'"
            ));
        }
        match flag {
            'i' | 'm' => prefix.push(flag),
            's' => dot_all = true,
            'u' | 'v' | 'y' => {
                return Err(invalid(&format!(
                    "the {flag} flag is not supported by this regex engine"
                )));
            }
            _ => {}
        }
    }
    let mut translator = Translator {
        chars: source.chars().collect(),
        pos: 0,
        out: if prefix.is_empty() {
            String::new()
        } else {
            format!("(?{prefix})")
        },
        dot_all,
    };
    translator.pattern().map_err(|reason| invalid(&reason))?;
    Regex::new(&translator.out).map_err(|e| {
        let text = e.to_string();
        // A syntax error is a drawing of the translated pattern with the reason
        // on its last line; only the reason means anything to the caller.
        let reason = text.lines().last().unwrap_or(&text);
        invalid(reason.trim().trim_start_matches("error: "))
    })
}

/// One member of a character class.
enum ClassItem {
    Char(char),
    /// An unescaped `-`: a range operator or a literal, depending on its neighbours.
    Dash,
    /// `\d`, `\W`, …: already in `regex` syntax.
    Set(&'static str),
}

struct Translator {
    chars: Vec<char>,
    pos: usize,
    out: String,
    dot_all: bool,
}

/// Writes `c` so that it can only be read as itself, inside or outside a class.
fn push_literal(out: &mut String, c: char) {
    if c.is_alphanumeric() {
        out.push(c);
    } else {
        out.push_str(&format!("\\x{{{:x}}}", u32::from(c)));
    }
}

impl Translator {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek(0)?;
        self.pos += 1;
        Some(c)
    }

    fn pattern(&mut self) -> Result<(), String> {
        while let Some(c) = self.next() {
            match c {
                '\\' => self.escape()?,
                '[' => self.class()?,
                '(' => {
                    if self.peek(0) == Some('?') {
                        let lookbehind = self.peek(1) == Some('<');
                        let sign = self.peek(if lookbehind { 2 } else { 1 });
                        if matches!(sign, Some('=' | '!')) {
                            let kind = if lookbehind {
                                "lookbehind"
                            } else {
                                "lookahead"
                            };
                            return Err(format!("{kind} is not supported by this regex engine"));
                        }
                    }
                    self.out.push('(');
                }
                '.' if self.dot_all => self.out.push_str("(?s:.)"),
                // JavaScript's line terminators.
                '.' => self.out.push_str(r"[^\n\r\x{2028}\x{2029}]"),
                '{' => match self.quantifier_len() {
                    Some(len) => {
                        self.out.push('{');
                        for _ in 0..len {
                            if let Some(c) = self.next() {
                                self.out.push(c);
                            }
                        }
                    }
                    // Not a quantifier: JavaScript reads the brace as itself.
                    None => self.out.push_str(r"\{"),
                },
                '}' => self.out.push_str(r"\}"),
                ']' => self.out.push_str(r"\]"),
                _ => self.out.push(c),
            }
        }
        Ok(())
    }

    /// Length of `n}`, `n,}` or `n,m}` at the cursor, which is just past a `{`.
    fn quantifier_len(&self) -> Option<usize> {
        let mut len = 0;
        let digits = |len: &mut usize| {
            let start = *len;
            while self.peek(*len).is_some_and(|c| c.is_ascii_digit()) {
                *len += 1;
            }
            *len > start
        };
        if !digits(&mut len) {
            return None;
        }
        if self.peek(len) == Some(',') {
            len += 1;
            digits(&mut len);
        }
        (self.peek(len) == Some('}')).then_some(len + 1)
    }

    /// The character an escape stands for, when it stands for one. `None`
    /// leaves the cursor where it was, on the character after the backslash.
    fn char_escape(&mut self) -> Result<Option<char>, String> {
        let Some(c) = self.peek(0) else {
            return Err("\\ at end of pattern".into());
        };
        let hex = |this: &Self, from: usize, count: usize| -> Option<u32> {
            let digits: String = (from..from + count).filter_map(|i| this.peek(i)).collect();
            (digits.len() == count && digits.chars().all(|d| d.is_ascii_hexdigit()))
                .then(|| u32::from_str_radix(&digits, 16).ok())
                .flatten()
        };
        let (value, consumed) = match c {
            't' => ('\t', 1),
            'n' => ('\n', 1),
            'r' => ('\r', 1),
            'f' => ('\u{c}', 1),
            'v' => ('\u{b}', 1),
            '0' if self.peek(1).is_some_and(|d| d.is_ascii_digit()) => {
                return Err("octal escapes are not supported by this regex engine".into());
            }
            '0' => ('\0', 1),
            'c' => match self.peek(1).filter(char::is_ascii_alphabetic) {
                Some(letter) => (char::from(letter as u8 & 31), 2),
                // `\c` without a letter is a backslash followed by "c".
                None => {
                    return Ok(Some('\\'));
                }
            },
            'x' => match hex(self, 1, 2).and_then(char::from_u32) {
                Some(value) => (value, 3),
                None => ('x', 1),
            },
            'u' => match hex(self, 1, 4) {
                Some(high @ 0xd800..=0xdbff) => {
                    let low = (self.peek(5) == Some('\\') && self.peek(6) == Some('u'))
                        .then(|| hex(self, 7, 4))
                        .flatten()
                        .filter(|low| (0xdc00..=0xdfff).contains(low));
                    let pair = low.and_then(|low| {
                        char::from_u32(0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00))
                    });
                    match pair {
                        Some(value) => (value, 11),
                        None => return Err("a lone surrogate cannot be matched".into()),
                    }
                }
                Some(unit) => match char::from_u32(unit) {
                    Some(value) => (value, 5),
                    None => return Err("a lone surrogate cannot be matched".into()),
                },
                None => ('u', 1),
            },
            _ => return Ok(None),
        };
        self.pos += consumed;
        Ok(Some(value))
    }

    fn escape(&mut self) -> Result<(), String> {
        if let Some(c) = self.char_escape()? {
            push_literal(&mut self.out, c);
            return Ok(());
        }
        let Some(c) = self.next() else {
            return Ok(());
        };
        match c {
            'd' => self.out.push_str(&format!("[{DIGIT}]")),
            'D' => self.out.push_str(&format!("[^{DIGIT}]")),
            'w' => self.out.push_str(&format!("[{WORD}]")),
            'W' => self.out.push_str(&format!("[^{WORD}]")),
            's' => self.out.push_str(JS_SPACE),
            'S' => self.out.push_str(JS_NOT_SPACE),
            // JavaScript's word characters are ASCII only.
            'b' => self.out.push_str(r"(?-u:\b)"),
            'B' => self.out.push_str(r"(?-u:\B)"),
            '1'..='9' => return Err(BACKREFERENCE.into()),
            'k' if self.peek(0) == Some('<') => return Err(BACKREFERENCE.into()),
            // Anything else escapes to itself (`\/`, `\<`, `\p`, `\A`), where the
            // `regex` crate would read an assertion or a Unicode class.
            _ => push_literal(&mut self.out, c),
        }
        Ok(())
    }

    fn class(&mut self) -> Result<(), String> {
        let negated = self.peek(0) == Some('^');
        if negated {
            self.pos += 1;
        }
        let mut items = Vec::new();
        loop {
            let Some(c) = self.next() else {
                return Err("Unterminated character class".into());
            };
            match c {
                ']' => break,
                '-' => items.push(ClassItem::Dash),
                '\\' => items.push(self.class_escape()?),
                _ => items.push(ClassItem::Char(c)),
            }
        }
        if items.is_empty() {
            // `[]` matches nothing and `[^]` anything; the `regex` crate would
            // read the `]` as a member instead.
            self.out
                .push_str(if negated { r"[\s\S]" } else { r"[^\s\S]" });
            return Ok(());
        }
        self.out.push('[');
        if negated {
            self.out.push('^');
        }
        let as_char = |item: Option<&ClassItem>| match item {
            Some(ClassItem::Char(c)) => Some(*c),
            Some(ClassItem::Dash) => Some('-'),
            _ => None,
        };
        let mut at = 0;
        while let Some(item) = items.get(at) {
            if let ClassItem::Set(set) = item {
                self.out.push_str(set);
                at += 1;
                continue;
            }
            let Some(low) = as_char(Some(item)) else {
                break;
            };
            let high = matches!(items.get(at + 1), Some(ClassItem::Dash))
                .then(|| as_char(items.get(at + 2)))
                .flatten();
            push_literal(&mut self.out, low);
            match high {
                Some(high) if high < low => {
                    return Err("Range out of order in character class".into());
                }
                Some(high) => {
                    self.out.push('-');
                    push_literal(&mut self.out, high);
                    // Without the `u` flag an astral character is two
                    // surrogate units for JavaScript: a range that spans the
                    // surrogate block (`[\u0080-\uffff]`) matches both.
                    if low <= '\u{d7ff}' && high >= '\u{e000}' {
                        self.out.push_str(r"\x{10000}-\x{10ffff}");
                    }
                    at += 3;
                }
                None => at += 1,
            }
        }
        self.out.push(']');
        Ok(())
    }

    fn class_escape(&mut self) -> Result<ClassItem, String> {
        if let Some(c) = self.char_escape()? {
            return Ok(ClassItem::Char(c));
        }
        let Some(c) = self.next() else {
            return Err("Unterminated character class".into());
        };
        Ok(match c {
            'd' => ClassItem::Set(DIGIT),
            'D' => ClassItem::Set("[^0-9]"),
            'w' => ClassItem::Set(WORD),
            'W' => ClassItem::Set("[^0-9A-Za-z_]"),
            's' => ClassItem::Set(JS_SPACE),
            'S' => ClassItem::Set(JS_NOT_SPACE),
            // Inside a class `\b` is a backspace.
            'b' => ClassItem::Char('\u{8}'),
            '1'..='9' => {
                return Err("octal escapes are not supported by this regex engine".into());
            }
            _ => ClassItem::Char(c),
        })
    }
}

const BACKREFERENCE: &str = "backreferences are not supported by this regex engine";

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(source: &str, flags: &str, text: &str) -> bool {
        js_regex(source, flags).unwrap().is_match(text)
    }

    fn found<'t>(source: &str, text: &'t str) -> Option<&'t str> {
        js_regex(source, "g")
            .unwrap()
            .find(text)
            .map(|m| m.as_str())
    }

    #[test]
    fn 숫자와_단어_클래스는_ascii만_잡는다() {
        assert!(matches(r"^\d+$", "", "0123"));
        // Arabic-Indic digits and Hangul are \d / \w for the regex crate, not for JavaScript.
        assert!(!matches(r"\d", "", "٣"));
        assert!(!matches(r"\w", "", "한"));
        assert!(matches(r"\W", "", "한"));
        assert!(matches(r"\D", "", "٣"));
        assert!(matches(r"^[\w.]+$", "", "a_b.c9"));
        assert!(matches(r"^[^\d]+$", "", "ab٣"));
    }

    #[test]
    fn 공백_클래스는_자바스크립트의_공백_집합이다() {
        assert!(matches(r"a\sb", "", "a\u{feff}b"));
        assert!(!matches(r"a\sb", "", "a\u{85}b"));
        assert!(matches(r"a\Sb", "", "a\u{85}b"));
        assert!(matches(r"^[\s,]+$", "", " ,\t\u{feff}"));
        assert!(matches(r"^[\S]+$", "", "ab\u{85}"));
    }

    #[test]
    fn 단어_경계는_ascii_단어_문자로만_판단한다() {
        assert_eq!(found(r"\bsign\b", "a.sign(x)"), Some("sign"));
        assert!(!matches(r"\bsign\b", "", "design"));
        // "한" is not a word character in JavaScript, so a boundary sits before "sign".
        assert!(matches(r"\bsign", "", "한sign"));
        assert!(matches(r"a\Bb", "", "ab"));
    }

    #[test]
    fn 점은_자바스크립트의_줄_끝_문자에서_멈춘다() {
        assert_eq!(found("a.*", "abc\r\ndef"), Some("abc"));
        assert_eq!(found("a.*", "abc\u{2028}def"), Some("abc"));
        assert_eq!(
            js_regex("a.*", "s")
                .unwrap()
                .find("abc\r\ndef")
                .unwrap()
                .as_str(),
            "abc\r\ndef"
        );
    }

    #[test]
    fn 플래그_i와_m을_옮긴다() {
        assert!(matches("hmac", "gi", "HMAC-SHA256"));
        assert!(!matches("hmac", "g", "HMAC-SHA256"));
        assert!(matches("^b$", "m", "a\nb\nc"));
        assert!(!matches("^b$", "", "a\nb\nc"));
    }

    #[test]
    fn 지원하지_않는_플래그와_잘못된_플래그는_에러다() {
        assert_eq!(
            js_regex("a", "gy").unwrap_err(),
            "Invalid regular expression: /a/gy: the y flag is not supported by this regex engine"
        );
        assert_eq!(
            js_regex("a", "gg").unwrap_err(),
            "Invalid flags supplied to RegExp constructor 'gg'"
        );
        assert_eq!(
            js_regex("a", "x").unwrap_err(),
            "Invalid flags supplied to RegExp constructor 'x'"
        );
    }

    #[test]
    fn 룩어라운드는_분명한_에러로_거절한다() {
        for (source, kind) in [
            ("a(?=b)", "lookahead"),
            ("a(?!b)", "lookahead"),
            ("(?<=a)b", "lookbehind"),
            ("(?<!a)b", "lookbehind"),
        ] {
            assert_eq!(
                js_regex(source, "g").unwrap_err(),
                format!(
                    "Invalid regular expression: /{source}/g: {kind} is not supported by this regex engine"
                )
            );
        }
    }

    #[test]
    fn 역참조는_분명한_에러로_거절한다() {
        let expected = "backreferences are not supported by this regex engine";
        assert!(js_regex(r"(a)\1", "g").unwrap_err().ends_with(expected));
        assert!(
            js_regex(r"(?<q>a)\k<q>", "g")
                .unwrap_err()
                .ends_with(expected)
        );
    }

    #[test]
    fn 이름_있는_그룹과_비캡처_그룹은_그대로_쓴다() {
        let re = js_regex(r"(?<key>\w+)=(?:\d+)", "g").unwrap();
        let caps = re.captures("token=42").unwrap();
        assert_eq!(caps.name("key").unwrap().as_str(), "token");
    }

    #[test]
    fn 문법_오류는_v8과_같은_모양의_메시지로_알린다() {
        let error = js_regex("(", "g").unwrap_err();
        assert!(
            error.starts_with("Invalid regular expression: /(/g: "),
            "{error}"
        );
        assert!(!error.contains('\n'), "{error}");
        assert_eq!(
            js_regex("[a", "").unwrap_err(),
            "Invalid regular expression: /[a/: Unterminated character class"
        );
        assert_eq!(
            js_regex("a\\", "").unwrap_err(),
            "Invalid regular expression: /a\\/: \\ at end of pattern"
        );
    }

    #[test]
    fn 수량자가_아닌_중괄호는_글자_그대로다() {
        assert!(matches(r"function\(\){", "", "function(){}"));
        assert!(matches("a{,5}", "", "a{,5}"));
        assert!(matches("^a{2,3}$", "", "aaa"));
        assert!(matches("^a{2}$", "", "aa"));
        assert!(matches("^a{2,}$", "", "aaaa"));
        assert!(matches(r#"\{"alg":"#, "", r#"{"alg":"HS256"}"#));
        assert!(matches("a]b}", "", "a]b}"));
    }

    #[test]
    fn 자기_자신을_뜻하는_이스케이프는_글자_그대로다() {
        // `\<` and `\A` are assertions for the regex crate, `\p` a Unicode class.
        assert!(matches(r"\<div\>", "", "<div>"));
        assert!(matches(r"^\A\p\z$", "", "Apz"));
        assert!(matches(r"https:\/\/a\.b", "", "https://a.b"));
        assert!(!matches(r"a\.b", "", "axb"));
        assert!(matches(r"\-\&\~", "", "-&~"));
    }

    #[test]
    fn 제어_문자와_코드_이스케이프를_옮긴다() {
        assert!(matches(r"a\tb\nc", "", "a\tb\nc"));
        assert!(matches(r"\x41\u0042\cJ", "", "AB\n"));
        assert!(matches(r"\uD83D\uDE00", "", "😀"));
        assert!(matches(r"a\0b", "", "a\0b"));
        assert!(matches(r"\xZZ", "", "xZZ"));
        assert!(js_regex(r"\uD83D", "").is_err());
        assert!(js_regex(r"\012", "").is_err());
    }

    #[test]
    fn 문자_클래스_안의_대괄호와_집합_연산자는_글자_그대로다() {
        assert!(matches(r"^[[a]+$", "", "[a[["));
        assert!(matches(r"^[a&&b]+$", "", "a&b"));
        assert!(matches(r"^[a~~b]+$", "", "a~b"));
        assert!(matches(r"^[[:alpha:]]$", "", ":]"));
        assert!(matches(r"^[\]\\]+$", "", "]\\"));
        assert!(matches(r"^[\b]$", "", "\u{8}"));
    }

    #[test]
    fn 문자_클래스의_범위와_하이픈을_자바스크립트처럼_읽는다() {
        assert!(matches("^[a-c]+$", "", "abc"));
        assert!(!matches("^[a-c]+$", "", "abd"));
        // A leading, trailing or escaped hyphen is a literal.
        assert!(matches("^[-a]+$", "", "-a"));
        assert!(matches("^[a-]+$", "", "a-"));
        assert!(matches(r"^[a\-c]+$", "", "a-c"));
        assert!(!matches(r"^[a\-c]+$", "", "b"));
        // `+--` is the range from "+" to "-", which holds ",".
        assert!(matches("^[+--]+$", "", "+,-"));
        // Next to a class escape the hyphen is a literal.
        assert!(matches(r"^[\d-z]+$", "", "5-z"));
        assert!(!matches(r"^[\d-z]+$", "", "a"));
        assert!(
            js_regex("[z-a]", "")
                .unwrap_err()
                .ends_with("Range out of order in character class")
        );
    }

    #[test]
    fn 서로게이트_구간을_덮는_범위는_bmp_밖_글자도_잡는다() {
        // Expected values are what the same patterns match under JavaScript.
        assert_eq!(found(r"[\u0080-\uffff]+", "a😀한b"), Some("😀한"));
        assert_eq!(found(r"[^\u0080-\uffff]+", "😀한b"), Some("b"));
        assert_eq!(found(r"[a-\uffff]+", "A😀zB"), Some("😀z"));
        // A range below the surrogates stays in the BMP.
        assert_eq!(found(r"[\u0080-\ud7ff]+", "a😀한b"), Some("한"));
    }

    #[test]
    fn 빈_클래스는_아무것도_또는_무엇이든_잡는다() {
        assert!(!matches("[]", "", "abc"));
        assert!(matches("a[^]b", "", "a\nb"));
    }

    #[test]
    fn 대소문자_무시와_클래스를_함께_쓴다() {
        assert!(matches("^[a-c]+$", "i", "AbC"));
        assert!(matches(r"x-amz-[\w-]+", "gi", "X-Amz-Signature"));
    }
}
