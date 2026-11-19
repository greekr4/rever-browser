//! Port of `src/renderer/src/stores/chat-draft.ts` — text another panel hands
//! to the chat input, taken exactly once.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatDraft {
    pending: Option<String>,
}

impl ChatDraft {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pending(&self) -> Option<&str> {
        self.pending.as_deref()
    }

    pub fn push(&mut self, text: impl Into<String>) {
        self.pending = Some(text.into());
    }

    pub fn consume(&mut self) -> Option<String> {
        self.pending.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 처음에는_대기_중인_초안이_없다() {
        let mut draft = ChatDraft::new();
        assert_eq!(draft.pending(), None);
        assert_eq!(draft.consume(), None);
    }

    #[test]
    fn consume은_초안을_한_번만_돌려준다() {
        let mut draft = ChatDraft::new();
        draft.push("explain this request");
        assert_eq!(draft.pending(), Some("explain this request"));
        assert_eq!(draft.consume().as_deref(), Some("explain this request"));
        assert_eq!(draft.consume(), None);
    }

    #[test]
    fn 다시_push하면_이전_초안을_덮어쓴다() {
        let mut draft = ChatDraft::new();
        draft.push("first");
        draft.push("second");
        assert_eq!(draft.consume().as_deref(), Some("second"));
    }

    #[test]
    fn 빈_문자열_초안도_초안으로_돌려준다() {
        let mut draft = ChatDraft::new();
        draft.push("");
        assert_eq!(draft.consume().as_deref(), Some(""));
    }
}
