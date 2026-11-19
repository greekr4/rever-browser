//! Port of `src/main/acp-turn.ts` — pure helpers for ACP prompt turn handling.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnResult<'a> {
    pub previous_prompt_failed: bool,
    pub update_count: usize,
    pub stop_reason: &'a str,
    pub elapsed_ms: u64,
}

/// claude-agent-acp leaves one stale `session_state_changed: idle` message in
/// the Claude Agent SDK query after a prompt rejects (e.g. an API error). The
/// next prompt consumes it immediately and resolves `end_turn` with zero
/// updates — the user sees an empty turn. Detect that case so the caller can
/// re-issue the prompt once.
pub fn is_stale_empty_turn(input: TurnResult) -> bool {
    if !input.previous_prompt_failed {
        return false;
    }
    if input.update_count > 0 {
        return false;
    }
    if input.stop_reason != "end_turn" {
        return false;
    }
    // A real model turn always takes longer than this; the stale one is ~0ms.
    input.elapsed_ms < 1_000
}

#[cfg(test)]
mod tests {
    use super::*;

    const STALE: TurnResult = TurnResult {
        previous_prompt_failed: true,
        update_count: 0,
        stop_reason: "end_turn",
        elapsed_ms: 3,
    };

    #[test]
    fn 직전_턴이_실패했고_업데이트_없이_즉시_end_turn이면_stale로_판정한다() {
        assert!(is_stale_empty_turn(STALE));
    }

    #[test]
    fn 직전_턴이_성공했으면_stale이_아니다() {
        assert!(!is_stale_empty_turn(TurnResult {
            previous_prompt_failed: false,
            ..STALE
        }));
    }

    #[test]
    fn 업데이트가_하나라도_있으면_stale이_아니다() {
        assert!(!is_stale_empty_turn(TurnResult {
            update_count: 1,
            ..STALE
        }));
    }

    #[test]
    fn cancelled_등_다른_stop_reason이면_stale이_아니다() {
        assert!(!is_stale_empty_turn(TurnResult {
            stop_reason: "cancelled",
            ..STALE
        }));
    }

    #[test]
    fn 일초_이상_걸린_빈_턴은_stale로_보지_않는다() {
        assert!(!is_stale_empty_turn(TurnResult {
            elapsed_ms: 2500,
            ..STALE
        }));
    }
}
