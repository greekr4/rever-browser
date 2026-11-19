//! The history panel and the bookmark bar
//!
//! [`history`] and [`bookmarks`] are the two stores, plain structs persisted through a
//! [`storage::Storage`]. [`history_panel`] and [`bookmark_bar`] are the GPUI views over
//! them; the host wraps each store in an `Entity`, hands it to the view, and subscribes
//! to [`OpenUrl`].

pub mod bookmark_bar;
pub mod bookmarks;
pub mod history;
pub mod history_panel;
pub mod storage;

use gpui::{Rgba, WindowAppearance, rgb, rgba};
use rever_ui_kit::theme::ThemeMode;

/// Emitted by both views when an entry is clicked: the URL to open. Stands in for
/// `useNavigationRequestStore().request(url)`; the host decides which tab navigates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenUrl(pub String);

/// `ui-monospace, monospace` in the stylesheet. GPUI resolves no generic families, and
/// Menlo is the monospace font every macOS has.
pub(crate) const MONO_FONT: &str = "Menlo";

/// Tokens of `src/renderer/src/styles.css` these views use that
/// `rever_ui_kit::theme::Theme` does not carry yet.
#[derive(Clone, Copy)]
pub(crate) struct LocalTokens {
    /// `--surface`
    pub surface: Rgba,
    /// `--glass-hover`
    pub glass_hover: Rgba,
}

impl LocalTokens {
    /// Follows `Theme::resolve`, so the local tokens never disagree with the shared ones.
    pub fn resolve(mode: ThemeMode, appearance: WindowAppearance) -> Self {
        let os_dark = matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        match mode {
            ThemeMode::Dark => Self::dark(),
            ThemeMode::System if os_dark => Self::dark(),
            _ => Self::light(),
        }
    }

    fn light() -> Self {
        Self {
            surface: rgb(0xffffff),
            // rgba(0, 0, 0, 0.05)
            glass_hover: rgba(0x0000000d),
        }
    }

    fn dark() -> Self {
        Self {
            surface: rgb(0x1a1a1a),
            // rgba(255, 255, 255, 0.06)
            glass_hover: rgba(0xffffff0f),
        }
    }
}
