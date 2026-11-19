//! Port of `src/renderer/src/stores/scan-mode.ts` — the one-shot scan latch.
//!
//! The Scan bar arms it; the transport consumes it on the next first-turn send
//! to prepend the recon persona exactly once — so a later, unrelated chat never
//! silently becomes a scan.

/// The tool profile a scan switches to.
pub const SCAN_TOOL_PROFILE: &str = "reverse";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanModeStore {
    armed: bool,
}

impl ScanModeStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn armed(&self) -> bool {
        self.armed
    }

    /// `set_profile` stands in for the tool-profile store's `setProfile`,
    /// which tells the main process; it is handed `SCAN_TOOL_PROFILE`.
    pub fn arm(&mut self, set_profile: impl FnOnce(&str)) {
        self.armed = true;
        // Scan needs list_endpoints/security_inspect/scan_secrets/finding_export,
        // which live in the reverse profile.
        set_profile(SCAN_TOOL_PROFILE);
    }

    pub fn consume(&mut self) -> bool {
        std::mem::take(&mut self.armed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- scan-mode.test.ts ---

    #[test]
    fn 기본은_무장_해제() {
        assert!(!ScanModeStore::new().consume());
    }

    #[test]
    fn arm_후_consume는_한_번만_true() {
        let mut store = ScanModeStore::new();
        store.arm(|_| {});
        assert!(store.consume());
        assert!(!store.consume());
    }

    // --- not covered by scan-mode.test.ts ---

    #[test]
    fn arm은_도구_프로필을_reverse로_바꾼다() {
        let mut store = ScanModeStore::new();
        let mut requested = Vec::new();
        store.arm(|profile| requested.push(profile.to_owned()));
        assert!(store.armed());
        assert_eq!(requested, ["reverse"]);
    }
}
