//! Port of `src/renderer/src/stores/viewport.ts` — the viewport the page is
//! shown in. Not persisted: every launch starts on desktop.

use serde::{Deserialize, Serialize};

/// `ViewportMode` of the preload API.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewportMode {
    #[default]
    Desktop,
    Mobile,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewportStore {
    mode: ViewportMode,
}

impl ViewportStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self) -> ViewportMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: ViewportMode) {
        self.mode = mode;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn 기본은_desktop이다() {
        assert_eq!(ViewportStore::new().mode(), ViewportMode::Desktop);
    }

    #[test]
    fn set_mode가_모드를_바꾼다() {
        let mut store = ViewportStore::new();
        store.set_mode(ViewportMode::Mobile);
        assert_eq!(store.mode(), ViewportMode::Mobile);
        store.set_mode(ViewportMode::Desktop);
        assert_eq!(store.mode(), ViewportMode::Desktop);
    }

    #[test]
    fn 모드는_소문자_문자열로_직렬화된다() {
        assert_eq!(json!(ViewportMode::Desktop), json!("desktop"));
        assert_eq!(json!(ViewportMode::Mobile), json!("mobile"));
        assert_eq!(
            serde_json::from_value::<ViewportMode>(json!("mobile")).unwrap(),
            ViewportMode::Mobile
        );
        assert!(serde_json::from_value::<ViewportMode>(json!("tablet")).is_err());
    }
}
