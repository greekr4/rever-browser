//! Port of `src/main/intercept-match.ts` — pure helpers for matching a paused
//! Fetch request against intercept rules.
//!
//! The TypeScript compiled each pattern to a `RegExp`. This port runs its own
//! small matcher instead, over UTF-16 code units, because that is the only way
//! to keep what the RegExp did at the edges: `?` and `*` count code units and
//! stop at line terminators, and `\` followed by a letter or digit meant
//! whatever a JavaScript regex makes of that escape (`\d`, `\b`, `\x41`, …).

use crate::is_js_whitespace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    /// One exact UTF-16 code unit.
    Unit(u16),
    /// `?` — the regex `.`: one code unit that is not a line terminator.
    AnyOne,
    /// `*` — the regex `.*`.
    AnyRun,
    /// `\d` `\w` `\s`, or the upper-case complement when `negated`.
    Class { class: Class, negated: bool },
    /// `\b` (`at_boundary`) or `\B`: consumes nothing.
    WordBoundary { at_boundary: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Digit,
    Word,
    Space,
}

/// A compiled CDP `Fetch.enable` urlPattern; the stand-in for the TypeScript's RegExp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlPattern {
    tokens: Vec<Token>,
}

fn ascii(unit: u16) -> Option<char> {
    u8::try_from(unit).ok().filter(u8::is_ascii).map(char::from)
}

fn is_word(unit: u16) -> bool {
    ascii(unit).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_line_terminator(unit: u16) -> bool {
    matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029)
}

impl Class {
    fn contains(self, unit: u16) -> bool {
        match self {
            Class::Digit => ascii(unit).is_some_and(|c| c.is_ascii_digit()),
            Class::Word => is_word(unit),
            Class::Space => char::from_u32(unit.into()).is_some_and(is_js_whitespace),
        }
    }
}

/// Reads the escape whose first unit sits at `at` (just past the backslash),
/// pushes what a JavaScript regex without the `u` flag matches for it, and
/// returns the index after it. The hex, octal and control forms swallow the
/// units that follow, which the TypeScript had copied into the regex source
/// unchanged. Anything unrecognised is the escaped unit itself.
fn push_escape(units: &[u16], at: usize, tokens: &mut Vec<Token>) -> usize {
    let Some(&unit) = units.get(at) else {
        return at;
    };
    let digit = |offset: usize, radix: u32| {
        units
            .get(at + offset)
            .and_then(|&u| ascii(u))
            .and_then(|c| c.to_digit(radix))
    };
    let number = |digits: &[u32], radix: u32| {
        // At most four hex digits, so the value always fits a code unit.
        let value = digits.iter().fold(0, |acc, d| acc * radix + d);
        Token::Unit(u16::try_from(value).unwrap_or(u16::MAX))
    };
    let class = |class, negated| Token::Class { class, negated };

    let mut len = 1;
    let token = match ascii(unit) {
        Some('d') => class(Class::Digit, false),
        Some('D') => class(Class::Digit, true),
        Some('w') => class(Class::Word, false),
        Some('W') => class(Class::Word, true),
        Some('s') => class(Class::Space, false),
        Some('S') => class(Class::Space, true),
        Some('b') => Token::WordBoundary { at_boundary: true },
        Some('B') => Token::WordBoundary { at_boundary: false },
        Some('f') => Token::Unit(0x0c),
        Some('n') => Token::Unit(0x0a),
        Some('r') => Token::Unit(0x0d),
        Some('t') => Token::Unit(0x09),
        Some('v') => Token::Unit(0x0b),
        Some('c') => match units
            .get(at + 1)
            .copied()
            .filter(|&u| ascii(u).is_some_and(|c| c.is_ascii_alphabetic()))
        {
            Some(letter) => {
                len = 2;
                Token::Unit(letter % 32)
            }
            // `\c` without a letter matches the backslash itself, then a plain `c`.
            None => {
                tokens.push(Token::Unit(u16::from(b'\\')));
                Token::Unit(unit)
            }
        },
        Some('x') => match (digit(1, 16), digit(2, 16)) {
            (Some(a), Some(b)) => {
                len = 3;
                number(&[a, b], 16)
            }
            _ => Token::Unit(unit),
        },
        Some('u') => match (digit(1, 16), digit(2, 16), digit(3, 16), digit(4, 16)) {
            (Some(a), Some(b), Some(c), Some(d)) => {
                len = 5;
                number(&[a, b, c, d], 16)
            }
            _ => Token::Unit(unit),
        },
        // Legacy octal: there are no capture groups to refer back to, so `\1`
        // is U+0001. Up to three digits, and never more than \377.
        Some('0'..='7') => match (digit(0, 8), digit(1, 8), digit(2, 8)) {
            (Some(a), Some(b), Some(c)) if a <= 3 => {
                len = 3;
                number(&[a, b, c], 8)
            }
            (Some(a), Some(b), _) => {
                len = 2;
                number(&[a, b], 8)
            }
            (Some(a), _, _) => number(&[a], 8),
            _ => Token::Unit(unit),
        },
        _ => Token::Unit(unit),
    };
    tokens.push(token);
    at + len
}

/// Compile a CDP `Fetch.enable` urlPattern.
/// CDP semantics: `*` = zero or more chars, `?` = exactly one, `\` escapes.
/// Every other character is literal, so agent input can never fail to compile.
pub fn compile_url_pattern(pattern: &str) -> UrlPattern {
    let units: Vec<u16> = pattern.encode_utf16().collect();
    let mut tokens = Vec::with_capacity(units.len());
    let mut i = 0;
    while let Some(&unit) = units.get(i) {
        i += 1;
        match ascii(unit) {
            // A trailing backslash has nothing to escape and is literal.
            Some('\\') if i < units.len() => i = push_escape(&units, i, &mut tokens),
            Some('*') => tokens.push(Token::AnyRun),
            Some('?') => tokens.push(Token::AnyOne),
            _ => tokens.push(Token::Unit(unit)),
        }
    }
    UrlPattern { tokens }
}

impl UrlPattern {
    /// Whether the whole of `url` matches (the RegExp was anchored at both ends).
    pub fn is_match(&self, url: &str) -> bool {
        let text: Vec<u16> = url.encode_utf16().collect();
        // reached[j]: the tokens so far can match exactly the first j units.
        // One pass over the text per token, so no pattern can backtrack badly.
        let mut reached = vec![false; text.len() + 1];
        let mut next = reached.clone();
        if let Some(start) = reached.first_mut() {
            *start = true;
        }
        for token in &self.tokens {
            match *token {
                Token::AnyRun => {
                    let consumed = std::iter::once(None).chain(text.iter().map(Some));
                    let mut open = false;
                    for ((slot, &here), unit) in next.iter_mut().zip(&reached).zip(consumed) {
                        open = here || (open && unit.is_some_and(|&u| !is_line_terminator(u)));
                        *slot = open;
                    }
                }
                Token::WordBoundary { at_boundary } => {
                    let before = std::iter::once(false).chain(text.iter().map(|&u| is_word(u)));
                    let after = text
                        .iter()
                        .map(|&u| is_word(u))
                        .chain(std::iter::once(false));
                    for ((slot, &here), (b, a)) in
                        next.iter_mut().zip(&reached).zip(before.zip(after))
                    {
                        *slot = here && ((b != a) == at_boundary);
                    }
                }
                single => {
                    if let Some(start) = next.first_mut() {
                        *start = false;
                    }
                    for ((slot, &here), &unit) in next.iter_mut().skip(1).zip(&reached).zip(&text) {
                        *slot = here
                            && match single {
                                Token::Unit(expected) => unit == expected,
                                Token::Class { class, negated } => class.contains(unit) != negated,
                                _ => !is_line_terminator(unit),
                            };
                    }
                }
            }
            if !next.contains(&true) {
                return false;
            }
            std::mem::swap(&mut reached, &mut next);
        }
        reached.last().copied().unwrap_or(false)
    }
}

#[derive(Clone, Debug)]
pub struct CompiledRule<R> {
    pub rule: R,
    pub re: UrlPattern,
}

/// The `{ urlPattern: string }` bound of the TypeScript's `compileRules`.
pub trait HasUrlPattern {
    fn url_pattern(&self) -> &str;
}

pub fn compile_rules<R: HasUrlPattern>(rules: Vec<R>) -> Vec<CompiledRule<R>> {
    rules
        .into_iter()
        .map(|rule| {
            let re = compile_url_pattern(rule.url_pattern());
            CompiledRule { rule, re }
        })
        .collect()
}

/// First rule whose pattern matches `url`, or None.
pub fn find_matching_rule<'a, R>(compiled: &'a [CompiledRule<R>], url: &str) -> Option<&'a R> {
    compiled
        .iter()
        .find(|c| c.re.is_match(url))
        .map(|c| &c.rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rule {
        id: &'static str,
        url_pattern: &'static str,
    }

    impl HasUrlPattern for Rule {
        fn url_pattern(&self) -> &str {
            self.url_pattern
        }
    }

    // compileUrlPattern

    #[test]
    fn 별표는_임의_길이의_문자열로_보고_전체를_앵커한다() {
        let re = compile_url_pattern("*/api/v1/*");
        assert!(re.is_match("https://x.com/api/v1/users"));
        assert!(!re.is_match("https://x.com/api/v2/users"));
    }

    #[test]
    fn 물음표는_정확히_한_글자로_본다() {
        let re = compile_url_pattern("https://x.com/item?");
        assert!(re.is_match("https://x.com/item1"));
        assert!(!re.is_match("https://x.com/item12"));
    }

    #[test]
    fn 짝이_안_맞는_정규식_메타문자에도_실패하지_않는다() {
        // Compiling cannot fail by construction; the pattern still has to match literally.
        let re = compile_url_pattern("*/api/(v1/*");
        assert!(re.is_match("https://x.com/api/(v1/z"));
        assert!(!re.is_match("https://x.com/api/v1/z"));
    }

    #[test]
    fn 점은_문자_그대로_본다() {
        assert!(!compile_url_pattern("https://x.com/*").is_match("https://xzcom/"));
    }

    #[test]
    fn 백슬래시_이스케이프를_따른다() {
        let re = compile_url_pattern("https://x.com/a\\*b");
        assert!(re.is_match("https://x.com/a*b"));
        assert!(!re.is_match("https://x.com/aXb"));
    }

    // findMatchingRule

    #[test]
    fn 순서대로_처음_맞는_규칙을_돌려준다() {
        let compiled = compile_rules(vec![
            Rule {
                id: "a",
                url_pattern: "*/other/*",
            },
            Rule {
                id: "b",
                url_pattern: "*/api/*",
            },
            Rule {
                id: "c",
                url_pattern: "*",
            },
        ]);
        assert_eq!(
            find_matching_rule(&compiled, "https://x.com/api/1").map(|r| r.id),
            Some("b")
        );
        assert_eq!(
            find_matching_rule(&compiled, "https://x.com/zzz").map(|r| r.id),
            Some("c")
        );
        assert!(find_matching_rule::<Rule>(&[], "https://x.com/zzz").is_none());
    }

    // Not in the TypeScript suite: each line was checked against the RegExp the
    // TypeScript builds for the same pattern (see the expected value beside it).

    #[test]
    fn 정규식이_하던_가장자리_동작을_그대로_따른다() {
        let cases: &[(&str, &str, bool)] = &[
            // `.` never matches a line terminator
            ("a*b", "a\nb", false),
            ("a?b", "a\u{2028}b", false),
            // `?` is one UTF-16 code unit, so an astral character needs two
            ("a?b", "a😀b", false),
            ("a??b", "a😀b", true),
            // a trailing backslash is literal
            ("a\\", "a\\", true),
            ("a\\\\b", "a\\b", true),
            ("a\\?b", "a?b", true),
            ("a\\?b", "aXb", false),
            // escapes that mean something to a JavaScript regex
            ("id=\\d", "id=7", true),
            ("id=\\d", "id=x", false),
            ("id=\\D", "id=x", true),
            ("\\w\\W\\s\\S", "a- b", true),
            ("\\s", "\u{feff}", true),
            ("\\s", "\u{85}", false),
            ("a\\b*", "a/b", true),
            ("a\\b*", "ab", false),
            ("a\\B*", "ab", true),
            ("a\\tb", "a\tb", true),
            ("\\x41\\u0042\\103", "ABC", true),
            ("\\x4", "x4", true),
            ("\\u004", "u004", true),
            ("\\0", "\0", true),
            ("\\08", "\u{0}8", true),
            ("\\8", "8", true),
            ("\\400", "\u{20}0", true),
            ("\\cA", "\u{1}", true),
            ("\\c1", "\\c1", true),
            ("\\q", "q", true),
            ("\\😀", "😀", true),
            // everything else is literal
            ("a+b$^{}()|[]/", "a+b$^{}()|[]/", true),
            ("", "", true),
            ("", "a", false),
            ("**", "", true),
            // an escape reads on into the units after it, or falls back to the unit itself
            ("\\c*", "\\cxyz", true),
            ("\\c\\A", "\\cA", true),
            ("\\c", "\\c", true),
            ("\\cj", "\n", true),
            ("\\cZ", "\u{1a}", true),
            ("\\k", "k", true),
            ("\\p", "p", true),
            ("\\-", "-", true),
            ("\\377", "\u{ff}", true),
            ("\\378", "\u{1f}8", true),
            ("\\777", "\u{3f}7", true),
            ("\\1\\0", "\u{1}\u{0}", true),
            ("\\00", "\u{0}", true),
            ("\\9", "9", true),
            ("\\x", "x", true),
            ("\\x4g", "x4g", true),
            ("\\uD83D\\uDE00", "😀", true),
            ("*\\b", "a", true),
            ("\\b*", "-", false),
            ("\\B*", "-", true),
            ("\\v\\f\\n\\r", "\u{b}\u{c}\n\r", true),
            ("a\\/b", "a/b", true),
            ("\\\n", "\n", true),
            // the classes are ASCII-only, as in a regex without the `u` flag
            ("\\w", "é", false),
            ("\\d", "٣", false),
            ("?", "\r", false),
        ];
        for &(pattern, url, expected) in cases {
            assert_eq!(
                compile_url_pattern(pattern).is_match(url),
                expected,
                "pattern {pattern:?} against {url:?}"
            );
        }
    }
}
