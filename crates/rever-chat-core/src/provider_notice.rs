//! Port of `src/renderer/src/ai/provider-notice.ts` — detects the model provider's
//! safeguard / model-fallback notice.
//!
//! The ACP agent streams the notice as plain `agent_message_chunk` text — there is
//! no structured stopReason for it (unlike a hard 'refusal'). The transport runs
//! this on the turn's full accumulated text at finish (not per-delta: the notice is
//! split across chunk boundaries mid-stream) to surface a distinct UI banner instead
//! of letting the notice blend into the answer.
//!
//! Matched against the stable phrasing the Claude Agent SDK emits, e.g.
//!   "Fable 5.1's safeguards stopped the response above …"
//!   "Model fallback: claude-fable-5-1 declined this request (cyber); retried …"
//!   "Opus 4.8's safeguards flagged this message."
//! Kept conservative (two-token signatures) so ordinary prose that merely
//! mentions "safeguards" or "fallback" does not trip it.
//!
//! The three TypeScript patterns are matched by hand rather than with the regex
//! crate: its `\s`, `\b` and case folding are Unicode-aware, and it counts the
//! `{0,120}` gap in code points where JavaScript counts UTF-16 units.

use crate::is_js_whitespace;

const MODEL_FALLBACK: &str = "model fallback";
const DECLINED: &str = "declined this request";

/// JavaScript's `\w` without the `u` flag.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Start offsets of `phrase` with a `\b` on both sides. The phrases start and end
/// with a letter, so a boundary is simply "no word character next to it".
fn bounded<'a>(text: &'a str, phrase: &'a str) -> impl Iterator<Item = usize> + 'a {
    text.match_indices(phrase).filter_map(move |(at, _)| {
        let before = text[..at].bytes().next_back();
        let after = text[at + phrase.len()..].bytes().next();
        (!before.is_some_and(is_word_byte) && !after.is_some_and(is_word_byte)).then_some(at)
    })
}

/// `[\s\S]{0,max}?` between two matches, counted in UTF-16 units like JavaScript.
fn gap_fits(gap: &str, max: usize) -> bool {
    gap.encode_utf16().take(max + 1).count() <= max
}

/// `/safeguards?\s+(?:stopped|flagged)\b/`
fn safeguard_stopped(text: &str) -> bool {
    text.match_indices("safeguard").any(|(at, word)| {
        let rest = &text[at + word.len()..];
        let rest = rest.strip_prefix('s').unwrap_or(rest);
        let verb = rest.trim_start_matches(is_js_whitespace);
        verb.len() < rest.len()
            && ["stopped", "flagged"].iter().any(|v| {
                verb.strip_prefix(v)
                    .is_some_and(|tail| !tail.bytes().next().is_some_and(is_word_byte))
            })
    })
}

/// `/\bmodel fallback\b[\s\S]{0,120}?\bdeclined this request\b/`
fn fallback_declined(text: &str) -> bool {
    bounded(text, MODEL_FALLBACK).any(|at| {
        let from = at + MODEL_FALLBACK.len();
        let rest = &text[from..];
        bounded(rest, DECLINED)
            .next()
            .is_some_and(|declined| gap_fits(&rest[..declined], 120))
    })
}

/// `/\bdeclined this request\b[\s\S]{0,40}?\(cyber\)/`
fn declined_cyber(text: &str) -> bool {
    bounded(text, DECLINED).any(|at| {
        let rest = &text[at + DECLINED.len()..];
        rest.find("(cyber)")
            .is_some_and(|cyber| gap_fits(&rest[..cyber], 40))
    })
}

pub fn detect_safeguard_notice(text: Option<&str>) -> bool {
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return false;
    };
    // The `i` flag without `u` folds ASCII letters only, and every pattern is ASCII.
    let text = text.to_ascii_lowercase();
    safeguard_stopped(&text) || fallback_declined(&text) || declined_cyber(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safeguard가_응답을_멈춘_노티스를_감지한다() {
        assert!(detect_safeguard_notice(Some(
            "Fable 5.1's safeguards stopped the response above · continuing once with that noted"
        )));
    }

    #[test]
    fn 모델_폴백_노티스를_감지한다() {
        assert!(detect_safeguard_notice(Some(
            "Model fallback: claude-fable-5-1 declined this request (cyber); retried with claude-opus-4-8."
        )));
    }

    #[test]
    fn safeguards_flagged_this_message_노티스를_감지한다() {
        assert!(detect_safeguard_notice(Some(
            "Opus 4.8's safeguards flagged this message."
        )));
    }

    #[test]
    fn 노티스가_답변_본문에_섞여_있어도_감지한다() {
        let mixed = "Sure, here is what I found.\n\nModel fallback: claude-fable-5-1 declined this request (cyber); retried with claude-opus-4-8.\n\nThe endpoint is /api/v2/search.";
        assert!(detect_safeguard_notice(Some(mixed)));
    }

    #[test]
    fn 일반_답변은_감지하지_않는다() {
        assert!(!detect_safeguard_notice(Some(
            "The login endpoint is POST /api/auth with a bearer token."
        )));
    }

    #[test]
    fn safeguard를_단순_언급한_문장은_감지하지_않는다() {
        assert!(!detect_safeguard_notice(Some(
            "You could add a safeguard here to validate the input before sending."
        )));
    }

    #[test]
    fn 빈_입력은_false() {
        assert!(!detect_safeguard_notice(Some("")));
        // undefined and null both arrive as None.
        assert!(!detect_safeguard_notice(None));
    }

    // The cases below are not in the TypeScript test file: they pin the regex
    // details the hand-written matcher has to reproduce.

    #[test]
    fn 단어_경계와_공백_규칙을_정규식과_같게_본다() {
        assert!(detect_safeguard_notice(Some("SAFEGUARD\u{FEFF}Flagged.")));
        assert!(!detect_safeguard_notice(Some("safeguards stoppedx")));
        assert!(!detect_safeguard_notice(Some("safeguardsstopped")));
        // U+0085 is whitespace for Rust but not for JavaScript's \s.
        assert!(!detect_safeguard_notice(Some("safeguards\u{85}stopped")));
        // A non-ASCII letter is not a word character, so the boundary holds.
        assert!(detect_safeguard_notice(Some("safeguards stopped가")));
        assert!(!detect_safeguard_notice(Some(
            "remodel fallback: x declined this request"
        )));
    }

    #[test]
    fn 간격_한도는_utf16_단위로_센다() {
        let within = format!("model fallback {} declined this request", "a".repeat(118));
        let beyond = format!("model fallback {} declined this request", "a".repeat(119));
        assert!(detect_safeguard_notice(Some(&within)));
        assert!(!detect_safeguard_notice(Some(&beyond)));

        // An astral character is two UTF-16 units: 20 of them fill the 40 allowed.
        let cyber = |gap: &str| format!("it declined this request{gap}(cyber)");
        assert!(detect_safeguard_notice(Some(&cyber(&"😀".repeat(20)))));
        assert!(!detect_safeguard_notice(Some(&cyber(
            &("😀".repeat(20) + "a")
        ))));
    }

    // Checked against the TypeScript with bun.
    #[test]
    fn 뒤쪽에서_다시_시작하는_일치와_아스키만_접는_대소문자를_본다() {
        // "undeclined" has no boundary, but the bounded phrase after it is in reach.
        assert!(detect_safeguard_notice(Some(
            "model fallback: undeclined this request then it declined this request"
        )));
        // The first "model fallback" is too far away; the second one matches.
        assert!(detect_safeguard_notice(Some(&format!(
            "model fallback {} model fallback: declined this request",
            "a".repeat(300)
        ))));
        assert!(detect_safeguard_notice(Some(&format!(
            "declined this request {} declined this request (cyber)",
            "y".repeat(50)
        ))));
        // No leading \b on the first pattern.
        assert!(detect_safeguard_notice(Some("xsafeguards stopped")));
        // U+200B is not \s, and "ſ" does not fold to "s" without the u flag.
        assert!(!detect_safeguard_notice(Some("safeguards\u{200B}stopped")));
        assert!(!detect_safeguard_notice(Some("ſafeguards stopped")));
        assert!(!detect_safeguard_notice(Some("safeguards ſtopped")));
    }
}
