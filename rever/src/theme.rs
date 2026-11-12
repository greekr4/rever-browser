//! Colors of the interface. Port of the `:root` token blocks in
//! `src/renderer/src/styles.css` (dark is the default there, light the override).

use gpui::{Rgba, WindowAppearance, rgb, rgba};

#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Rgba,
    pub bg_bar: Rgba,
    pub surface_2: Rgba,
    pub surface_3: Rgba,
    pub border: Rgba,
    pub border_2: Rgba,
    pub text: Rgba,
    pub text_2: Rgba,
    pub text_dim: Rgba,
    pub accent: Rgba,
    pub viewport_bg: Rgba,
    pub chrome_frame: Rgba,
    pub chrome_toolbar: Rgba,
    pub chrome_omnibox: Rgba,
    pub chrome_hover: Rgba,
    pub http_none: Rgba,
    pub http_2xx: Rgba,
    pub http_3xx: Rgba,
    pub http_4xx: Rgba,
    pub http_5xx: Rgba,
    pub status_error: Rgba,
    pub status_warn: Rgba,
}

/// Port of the `mode` in `src/renderer/src/stores/app-theme.ts`: light by default,
/// `System` follows the OS.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ThemeMode {
    System,
    #[default]
    Light,
    Dark,
}

impl ThemeMode {
    /// `REVER_THEME=system|light|dark`; the setting has no menu in the Rust app yet.
    pub fn from_env() -> Self {
        match std::env::var("REVER_THEME")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "system" => Self::System,
            "dark" => Self::Dark,
            _ => Self::Light,
        }
    }
}

impl Theme {
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

    pub fn light() -> Self {
        Self {
            bg: rgb(0xfbfbfc),
            bg_bar: rgb(0xeef0f2),
            surface_2: rgb(0xf0f1f3),
            surface_3: rgb(0xe6e8ea),
            border: rgb(0xe2e4e8),
            border_2: rgb(0xd3d6db),
            text: rgb(0x1b1d21),
            text_2: rgb(0x3f444b),
            text_dim: rgb(0x61676e),
            accent: rgb(0x2f6fed),
            viewport_bg: rgb(0xffffff),
            chrome_frame: rgb(0xdfe3e8),
            chrome_toolbar: rgb(0xffffff),
            chrome_omnibox: rgb(0xe9eef6),
            chrome_hover: rgba(0x00000012),
            http_none: rgb(0x61676e),
            http_2xx: rgb(0x1f7a34),
            http_3xx: rgb(0x6b3fa8),
            http_4xx: rgb(0x8a6100),
            http_5xx: rgb(0xc62828),
            status_error: rgb(0xc62828),
            status_warn: rgb(0x8a6100),
        }
    }

    pub fn dark() -> Self {
        Self {
            bg: rgb(0x0e0e0e),
            bg_bar: rgb(0x161616),
            surface_2: rgb(0x222222),
            surface_3: rgb(0x2a2a2a),
            border: rgb(0x2a2a2a),
            border_2: rgb(0x333333),
            text: rgb(0xe6e6e6),
            text_2: rgb(0xcccccc),
            text_dim: rgb(0x999999),
            accent: rgb(0x4a8fff),
            viewport_bg: rgb(0x000000),
            chrome_frame: rgb(0x1b1c1e),
            chrome_toolbar: rgb(0x2a2b2e),
            chrome_omnibox: rgb(0x1b1c1e),
            chrome_hover: rgba(0xffffff1a),
            http_none: rgb(0x909090),
            http_2xx: rgb(0x99dd99),
            http_3xx: rgb(0xaa99ff),
            http_4xx: rgb(0xeecc99),
            http_5xx: rgb(0xff5555),
            status_error: rgb(0xff6b6b),
            status_warn: rgb(0xffd93d),
        }
    }

    pub fn http_status(&self, status: Option<u16>) -> Rgba {
        match status {
            Some(200..=299) => self.http_2xx,
            Some(300..=399) => self.http_3xx,
            Some(400..=499) => self.http_4xx,
            Some(500..) => self.http_5xx,
            _ => self.http_none,
        }
    }
}
