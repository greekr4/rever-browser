//! The console and exceptions panels

mod command_input;
pub mod console_panel;
pub mod exceptions_panel;

use chrono::{Local, TimeZone, Timelike};
use gpui::{Div, Rgba, WindowAppearance, div, prelude::*, px, rgb};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};

/// `fontFamily: 'ui-monospace, monospace'`. Menlo ships with every macOS; GPUI cannot
/// resolve the CSS generic names.
pub(crate) const MONO: &str = "Menlo";

/// `--status-info` and `--status-error-dim` from `src/renderer/src/styles.css`, which
/// `rever_ui_kit::theme::Theme` does not carry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusTokens {
    pub info: Rgba,
    pub error_dim: Rgba,
}

impl StatusTokens {
    pub fn light() -> Self {
        Self {
            info: rgb(0x1a5fb4),
            error_dim: rgb(0x9e2b25),
        }
    }

    pub fn dark() -> Self {
        Self {
            info: rgb(0x74b9ff),
            error_dim: rgb(0xff9999),
        }
    }

    /// Same choice as `Theme::resolve`.
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
}

/// A time of day as `toLocaleTimeString()` prints it for en-US and ko-KR.
pub fn clock(hour: u32, minute: u32, second: u32, lang: Lang) -> String {
    let hour12 = match hour % 12 {
        0 => 12,
        h => h,
    };
    let pm = hour % 24 >= 12;
    match lang {
        // Chrome separates the day period with a narrow no-break space.
        Lang::En => format!(
            "{hour12}:{minute:02}:{second:02}\u{202f}{}",
            if pm { "PM" } else { "AM" }
        ),
        Lang::Ko => format!(
            "{} {hour12}:{minute:02}:{second:02}",
            if pm { "오후" } else { "오전" }
        ),
    }
}

/// `new Date(ts).toLocaleTimeString()` in the machine's time zone.
pub fn format_time(ts: u64, lang: Lang) -> String {
    i64::try_from(ts)
        .ok()
        .and_then(|ms| Local.timestamp_millis_opt(ms).single())
        .map_or_else(
            || "Invalid Date".to_owned(),
            |time| clock(time.hour(), time.minute(), time.second(), lang),
        )
}

/// `.panel-empty`: the "nothing here yet" placeholder.
pub(crate) fn panel_empty(text: &'static str, theme: &Theme) -> Div {
    div()
        .px(px(12.0))
        .py(px(24.0))
        .flex()
        .justify_center()
        .text_color(theme.text_dim)
        .text_size(px(11.0))
        .child(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 영어는_12시간제_뒤에_오전_오후를_붙인다() {
        assert_eq!(clock(15, 4, 5, Lang::En), "3:04:05\u{202f}PM");
        assert_eq!(clock(9, 30, 0, Lang::En), "9:30:00\u{202f}AM");
    }

    #[test]
    fn 한국어는_오전_오후를_앞에_붙인다() {
        assert_eq!(clock(15, 4, 5, Lang::Ko), "오후 3:04:05");
        assert_eq!(clock(9, 30, 0, Lang::Ko), "오전 9:30:00");
    }

    #[test]
    fn 자정과_정오는_12시로_쓴다() {
        assert_eq!(clock(0, 0, 0, Lang::En), "12:00:00\u{202f}AM");
        assert_eq!(clock(12, 0, 0, Lang::En), "12:00:00\u{202f}PM");
        assert_eq!(clock(0, 0, 0, Lang::Ko), "오전 12:00:00");
        assert_eq!(clock(12, 0, 0, Lang::Ko), "오후 12:00:00");
    }

    #[test]
    fn 표현할_수_없는_시각은_자바스크립트처럼_invalid_date다() {
        assert_eq!(format_time(u64::MAX, Lang::En), "Invalid Date");
    }

    #[test]
    fn 밝은_테마와_어두운_테마의_상태색이_다르다() {
        let light = StatusTokens::resolve(ThemeMode::Light, WindowAppearance::Dark);
        let dark = StatusTokens::resolve(ThemeMode::System, WindowAppearance::Dark);
        assert_eq!(light, StatusTokens::light());
        assert_eq!(dark, StatusTokens::dark());
        assert_eq!(
            StatusTokens::resolve(ThemeMode::System, WindowAppearance::Light),
            StatusTokens::light()
        );
        assert_eq!(
            StatusTokens::resolve(ThemeMode::Dark, WindowAppearance::Light),
            StatusTokens::dark()
        );
    }
}
