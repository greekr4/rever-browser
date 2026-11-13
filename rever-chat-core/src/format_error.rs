//! Port of `src/renderer/src/ai/format-error.ts` — pure error-text formatting for chat turns.

/// `msg` is the error's message: the TypeScript took an `unknown` and read
/// `e.message` or `String(e)`, which in Rust is the caller's `err.to_string()`.
pub fn format_connection_error(msg: &str) -> String {
    if msg.contains("ENOENT") || msg.contains("spawn") {
        return format!("Agent binary not found. Check your PATH. ({msg})");
    }
    if msg.contains("timeout") || msg.contains("Timeout") {
        return "ACP server did not respond in time.".to_string();
    }
    // `/rate limit/i` without the `u` flag folds ASCII letters only.
    if msg.to_ascii_lowercase().contains("rate limit") {
        // claude-agent-acp reports an unavailable model (e.g. sonnet[1m] on an
        // account without 1M-context / extra-usage access) as a bare
        // "API Error: Rate limit reached". Say what to do about it.
        return format!(
            "{msg}. The selected model may not be available on this account (1M-context models need extra-usage access) — switch model or wait and retry."
        );
    }
    msg.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_에러에는_모델_사용_불가_안내를_덧붙인다() {
        let out = format_connection_error("Internal error: API Error: Rate limit reached");
        assert!(out.contains("Rate limit reached"));
        assert!(out.contains("switch model"));
    }

    #[test]
    fn enoent는_바이너리_누락_안내로_바꾼다() {
        assert!(format_connection_error("spawn claude-agent-acp ENOENT").contains("not found"));
    }

    #[test]
    fn 그_외_메시지는_그대로_돌려준다() {
        assert_eq!(format_connection_error("boom"), "boom");
    }

    // Not in the TypeScript test file: the timeout branch and the branch order.
    #[test]
    fn timeout은_응답_지연_안내로_바꾸고_spawn이_먼저_걸린다() {
        assert_eq!(
            format_connection_error("Request Timeout"),
            "ACP server did not respond in time."
        );
        assert_eq!(
            format_connection_error("spawn timeout"),
            "Agent binary not found. Check your PATH. (spawn timeout)"
        );
    }
}
