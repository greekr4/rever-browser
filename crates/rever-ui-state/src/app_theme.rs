//! Port of `src/renderer/src/stores/app-theme.ts` — the app-level UI theme.
//!
//! Distinct from the per-origin *webview* theme (webview_theme.rs) which
//! recolors the loaded site — this themes the rever-browser chrome itself.
//! `System` follows the OS color scheme.

use std::io;

use serde::{Deserialize, Serialize};

use crate::storage::{Storage, field, load_persisted, save_persisted};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResolvedTheme {
    Light,
    Dark,
}

/// The Electron app's bootstrap script reads the same key to apply the theme
/// before first paint, so the name and the stored shape must not change.
pub const APP_THEME_STORAGE_KEY: &str = "rev:app-theme";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppThemeStore {
    mode: ThemeMode,
}

impl AppThemeStore {
    pub fn load(storage: &dyn Storage) -> Self {
        let state = load_persisted(storage, APP_THEME_STORAGE_KEY);
        Self {
            // Nothing saved yet: follow the OS, as a browser does on first run.
            mode: field(&state, "mode").unwrap_or(ThemeMode::System),
        }
    }

    pub fn mode(&self) -> ThemeMode {
        self.mode
    }

    /// The state changes even when the write fails, as with `persist`.
    pub fn set_mode(&mut self, storage: &dyn Storage, mode: ThemeMode) -> io::Result<()> {
        self.mode = mode;
        save_persisted(storage, APP_THEME_STORAGE_KEY, self)
    }

    /// Cycle System → Light → Dark → System.
    pub fn cycle(&mut self, storage: &dyn Storage) -> io::Result<()> {
        let next = match self.mode {
            ThemeMode::System => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::System,
        };
        self.set_mode(storage, next)
    }
}

/// `system_prefers_dark` is the OS color scheme, which the caller asks the
/// platform for (the TypeScript read it from a media query).
pub fn resolve_theme(mode: ThemeMode, system_prefers_dark: bool) -> ResolvedTheme {
    match mode {
        ThemeMode::System if system_prefers_dark => ResolvedTheme::Dark,
        ThemeMode::System | ThemeMode::Light => ResolvedTheme::Light,
        ThemeMode::Dark => ResolvedTheme::Dark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn cycle은_system_light_dark_순서로_돈다() {
        let storage = MemoryStorage::new();
        let mut store = AppThemeStore::load(&storage);
        store.cycle(&storage).unwrap();
        assert_eq!(store.mode(), ThemeMode::Light);
        store.cycle(&storage).unwrap();
        assert_eq!(store.mode(), ThemeMode::Dark);
        store.cycle(&storage).unwrap();
        assert_eq!(store.mode(), ThemeMode::System);
    }

    #[test]
    fn 저장된_값이_없으면_시스템을_따른다() {
        assert_eq!(
            AppThemeStore::load(&MemoryStorage::new()).mode(),
            ThemeMode::System
        );
    }

    #[test]
    fn 모드를_persist_형식으로_저장하고_다시_불러온다() {
        let storage = MemoryStorage::new();
        let mut store = AppThemeStore::load(&storage);
        store.set_mode(&storage, ThemeMode::System).unwrap();
        assert_eq!(
            storage.get("rev:app-theme").as_deref(),
            Some(r#"{"state":{"mode":"system"},"version":0}"#)
        );
        assert_eq!(AppThemeStore::load(&storage).mode(), ThemeMode::System);
        store.cycle(&storage).unwrap();
        assert_eq!(AppThemeStore::load(&storage).mode(), ThemeMode::Light);
    }

    #[test]
    fn 알_수_없는_저장값은_기본_테마로_둔다() {
        let storage = MemoryStorage::new();
        for raw in [
            r#"{"state":{"mode":"sepia"},"version":0}"#,
            r#"{"state":{"mode":null},"version":0}"#,
            r#"{"state":{"mode":"dark"},"version":2}"#,
            "dark",
        ] {
            storage.set("rev:app-theme", raw).unwrap();
            assert_eq!(
                AppThemeStore::load(&storage).mode(),
                ThemeMode::System,
                "{raw}"
            );
        }
    }

    #[test]
    fn system_모드만_os_설정을_따른다() {
        assert_eq!(resolve_theme(ThemeMode::System, true), ResolvedTheme::Dark);
        assert_eq!(
            resolve_theme(ThemeMode::System, false),
            ResolvedTheme::Light
        );
        assert_eq!(resolve_theme(ThemeMode::Light, true), ResolvedTheme::Light);
        assert_eq!(resolve_theme(ThemeMode::Dark, false), ResolvedTheme::Dark);
    }
}
