//! Port of `src/main/mcp/grep-worker.ts` — the regex search that grep runs off
//! the caller's thread, and the error for a search that overran its budget.
//!
//! The TypeScript evaluates a worker script and terminates it on timeout. Here
//! the search is the plain function `exec_regex`; `script_analysis` runs it on a
//! `std::thread` with the timeout. A thread cannot be terminated, so the search
//! checks a cancel flag between matches instead.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use regex::{Match, Regex};

// Running an agent-supplied regex against a multi-megabyte script body on the
// caller's thread is a denial-of-service risk in the TypeScript: one
// catastrophic-backtracking pattern (e.g. /(a+)+$/) blocks the whole browser UI
// indefinitely, because a single RegExp.exec call cannot be interrupted. The
// `regex` crate never backtracks, but a search is still linear in a body of
// megabytes and repeats per match, so the wall-clock budget stays.

/// Wall-clock budget of one search, in milliseconds.
pub const GREP_TIMEOUT_MS: u64 = 5000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawMatch {
    /// Offset in UTF-16 code units, as JavaScript's `match.index`.
    pub index: usize,
    /// The same position as a byte offset into the body.
    pub byte_index: usize,
    /// The matched text (`match` in the TypeScript).
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrepTimeoutError {
    pub ms: u64,
}

impl fmt::Display for GrepTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pattern timed out after {}ms (possible catastrophic backtracking)",
            self.ms
        )
    }
}

impl std::error::Error for GrepTimeoutError {}

/// `re.exec(body)` in a loop with the global flag: every match in order, empty
/// ones included, stepping one character past an empty match as the
/// TypeScript's `re.lastIndex++` does.
pub(crate) fn exec_all<'b>(body: &'b str, regex: &'b Regex) -> impl Iterator<Item = Match<'b>> {
    let mut last_index = 0;
    std::iter::from_fn(move || {
        if last_index > body.len() {
            return None;
        }
        let m = regex.find_at(body, last_index)?;
        last_index = m.end();
        if m.is_empty() {
            let next = body.get(m.end()..).and_then(|rest| rest.chars().next());
            last_index += next.map_or(1, char::len_utf8);
        }
        Some(m)
    })
}

/// Turns ascending byte offsets into UTF-16 offsets without recounting the
/// body from the start for every match.
pub(crate) struct Utf16Offsets<'b> {
    body: &'b str,
    byte: usize,
    units: usize,
}

impl<'b> Utf16Offsets<'b> {
    pub(crate) fn new(body: &'b str) -> Self {
        Self {
            body,
            byte: 0,
            units: 0,
        }
    }

    pub(crate) fn at(&mut self, byte: usize) -> usize {
        if let Some(skipped) = self.body.get(self.byte..byte) {
            self.units += crate::utf16_len(skipped);
            self.byte = byte;
        }
        self.units
    }
}

/// Collects up to `max` raw matches of `regex` in `body`; the caller does the
/// cheap dedup / snippet slicing (dedup happens later, so pass a generous
/// multiple of the caller's real limit). Stops early once `cancelled` is set.
pub fn exec_regex(body: &str, regex: &Regex, max: usize, cancelled: &AtomicBool) -> Vec<RawMatch> {
    let mut out = Vec::new();
    let mut offsets = Utf16Offsets::new(body);
    for m in exec_all(body, regex) {
        if out.len() >= max || cancelled.load(Ordering::Relaxed) {
            break;
        }
        if m.is_empty() {
            continue;
        }
        out.push(RawMatch {
            index: offsets.at(m.start()),
            byte_index: m.start(),
            text: m.as_str().to_owned(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(body: &str, pattern: &str, max: usize) -> Vec<RawMatch> {
        exec_regex(
            body,
            &Regex::new(pattern).unwrap(),
            max,
            &AtomicBool::new(false),
        )
    }

    fn texts(matches: &[RawMatch]) -> Vec<&str> {
        matches.iter().map(|m| m.text.as_str()).collect()
    }

    #[test]
    fn 모든_매치를_순서대로_모은다() {
        let found = run("abcTOKENdefTOKENghi", "TOKEN", 10);
        assert_eq!(texts(&found), ["TOKEN", "TOKEN"]);
        assert_eq!(found[0].index, 3);
        assert_eq!(found[1].index, 11);
    }

    #[test]
    fn max에서_수집을_멈춘다() {
        assert_eq!(run("a a a a a", "a", 3).len(), 3);
        assert_eq!(run("a a a", "a", 0).len(), 0);
    }

    #[test]
    fn 빈_매치는_건너뛰고_한_글자씩_전진한다() {
        // /x*/g matches the empty string at every other position.
        assert_eq!(texts(&run("abxxcx", "x*", 10)), ["xx", "x"]);
        assert_eq!(run("가나다", "x*", 10), []);
        assert_eq!(run("", "x*", 10), []);
    }

    #[test]
    fn index는_utf16_단위이고_byte_index는_바이트다() {
        // "가" is one UTF-16 unit and three bytes, "😀" two units and four bytes.
        let found = run("가😀TOKEN나TOKEN", "TOKEN", 10);
        assert_eq!(
            found
                .iter()
                .map(|m| (m.index, m.byte_index))
                .collect::<Vec<_>>(),
            [(3, 7), (9, 15)]
        );
    }

    #[test]
    fn 직전_문맥을_보는_패턴도_위치를_옮겨가며_맞게_찾는다() {
        // \b after the first match must still see the preceding character.
        assert_eq!(
            texts(&run("useA useB xuseC", r"\buse[A-Z]", 10)),
            ["useA", "useB"]
        );
    }

    #[test]
    fn 취소_플래그가_서면_더_찾지_않는다() {
        let regex = Regex::new("a").unwrap();
        assert_eq!(exec_regex("a a a", &regex, 10, &AtomicBool::new(true)), []);
    }

    #[test]
    fn 타임아웃_오류_메시지에_제한_시간을_담는다() {
        assert_eq!(
            GrepTimeoutError { ms: 5000 }.to_string(),
            "pattern timed out after 5000ms (possible catastrophic backtracking)"
        );
    }
}
