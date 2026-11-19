//! Port of `src/renderer/src/stores/navigation-request.ts` — a navigation
//! asked for from outside the browser pane, waiting for the pane to pick it up.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingNavigation {
    pub url: String,
    /// When it was requested, in milliseconds since the epoch. Makes a repeat
    /// request for the same URL a new value.
    pub ts: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavigationRequestStore {
    pending: Option<PendingNavigation>,
}

impl NavigationRequestStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pending(&self) -> Option<&PendingNavigation> {
        self.pending.as_ref()
    }

    /// `now_ms` stands in for `Date.now()`.
    pub fn request(&mut self, url: impl Into<String>, now_ms: u64) {
        self.pending = Some(PendingNavigation {
            url: url.into(),
            ts: now_ms,
        });
    }

    pub fn clear(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 처음에는_대기_중인_이동이_없다() {
        assert_eq!(NavigationRequestStore::new().pending(), None);
    }

    #[test]
    fn request는_url과_시각을_담고_clear는_비운다() {
        let mut store = NavigationRequestStore::new();
        store.request("https://a.test", 1_000);
        assert_eq!(
            store.pending(),
            Some(&PendingNavigation {
                url: "https://a.test".to_owned(),
                ts: 1_000
            })
        );
        store.clear();
        assert_eq!(store.pending(), None);
    }

    #[test]
    fn 같은_url을_다시_요청하면_시각이_달라_새_요청이_된다() {
        let mut store = NavigationRequestStore::new();
        store.request("https://a.test", 1_000);
        let first = store.pending().cloned();
        store.request("https://a.test", 2_000);
        assert_ne!(store.pending().cloned(), first);
        assert_eq!(store.pending().map(|p| p.ts), Some(2_000));
    }

    #[test]
    fn 새_요청은_이전_요청을_대체한다() {
        let mut store = NavigationRequestStore::new();
        store.request("https://a.test", 1_000);
        store.request("https://b.test", 1_001);
        assert_eq!(
            store.pending().map(|p| p.url.as_str()),
            Some("https://b.test")
        );
    }
}
