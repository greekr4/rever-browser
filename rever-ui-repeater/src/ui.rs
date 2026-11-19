//! Pieces the two panels share: the color tokens the kit's theme does not carry yet, the
//! button styles of styles.css, the `title` tooltip, and a stand-in for `<select>`.

use gpui::{
    AnyView, App, BoxShadow, Div, ElementId, Hsla, Rgba, SharedString, Stateful, Window, div,
    point, prelude::*, px, rgb, rgba,
};
use rever_ui_kit::theme::{Theme, ThemeMode};

/// Stands in for the CSS `ui-monospace, monospace`.
pub(crate) const MONO: &str = ".AppleSystemUIFontMonospaced";
pub(crate) const SANS: &str = ".SystemUIFont";
/// The browser's `normal` line height at 11px.
pub(crate) const LINE_HEIGHT: f32 = 13.0;

/// The kit's theme plus the styles.css tokens it lacks.
#[derive(Clone, Copy)]
pub(crate) struct Tokens {
    pub theme: Theme,
    /// `--surface`
    pub surface: Rgba,
    /// `--row-active`
    pub row_active: Rgba,
    /// `--status-ok`
    pub status_ok: Rgba,
    /// `--chip-danger-bg`
    pub chip_danger_bg: Rgba,
    /// `--shadow`
    pub shadow: Rgba,
}

impl Tokens {
    pub fn resolve(mode: ThemeMode, window: &Window) -> Self {
        Self::of(Theme::resolve(mode, window.appearance()))
    }

    pub fn of(theme: Theme) -> Self {
        // The kit decides light or dark; this only asks which one it picked.
        if theme.bg == Theme::dark().bg {
            Self {
                theme,
                surface: rgb(0x1a1a1a),
                row_active: rgb(0x1d2a3a),
                status_ok: rgb(0x6ee7b7),
                chip_danger_bg: rgb(0x3a1f1f),
                shadow: rgba(0x00000066),
            }
        } else {
            Self {
                theme,
                surface: rgb(0xffffff),
                row_active: rgb(0xe3edfa),
                status_ok: rgb(0x1f7a34),
                chip_danger_bg: rgb(0xfbeaea),
                shadow: rgba(0x00000029),
            }
        }
    }
}

/// A div with an id. The view tests find it by that id; elsewhere this is `div().id(id)`.
pub(crate) fn clickable(id: impl Into<ElementId>) -> Stateful<Div> {
    let id = id.into();
    let selector = id.clone();
    div().id(id).debug_selector(move || selector.to_string())
}

/// `filter: brightness(1.1)`.
pub(crate) fn brighten(color: Rgba) -> Rgba {
    Rgba {
        r: (color.r * 1.1).min(1.0),
        g: (color.g * 1.1).min(1.0),
        b: (color.b * 1.1).min(1.0),
        a: color.a,
    }
}

/// The role classes of a `<button>` in styles.css.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ButtonKind {
    /// A plain `<button>`: the secondary action.
    Plain,
    /// `.btn-ghost`: no fill until hover.
    Ghost,
    /// `.btn-primary`: the main action.
    Primary,
    /// `.btn-danger.btn-icon`: a destructive icon.
    DangerIcon,
}

/// A `<button>`. A disabled one is dimmed and takes no hover style; the caller leaves
/// its click handler off.
pub(crate) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
    t: &Tokens,
) -> Stateful<Div> {
    let theme = t.theme;
    let base = clickable(id)
        .flex_none()
        .rounded(px(4.0))
        .font_family(SANS)
        .text_size(px(12.0))
        .line_height(px(16.0))
        .child(label.into());
    let base = if kind == ButtonKind::DangerIcon {
        base.min_w(px(22.0))
            .p(px(3.0))
            .flex()
            .items_center()
            .justify_center()
    } else {
        base.px(px(8.0)).py(px(3.0))
    };
    let base = match kind {
        ButtonKind::Plain => base.bg(theme.surface_3).text_color(theme.text),
        ButtonKind::Ghost => base.text_color(theme.text_2),
        ButtonKind::Primary => base.bg(theme.accent).text_color(rgb(0xffffff)),
        ButtonKind::DangerIcon => base.text_color(theme.status_error),
    };
    if !enabled {
        return base.opacity(0.42);
    }
    let danger_hover = t.chip_danger_bg;
    base.cursor_pointer().hover(move |style| match kind {
        ButtonKind::Plain => style.bg(theme.border_2),
        ButtonKind::Ghost => style.bg(theme.chrome_hover).text_color(theme.text),
        ButtonKind::Primary => style.bg(brighten(theme.accent)),
        ButtonKind::DangerIcon => style.bg(danger_hover),
    })
}

/// `.panel-empty`
pub(crate) fn empty(text: impl Into<SharedString>, t: &Tokens) -> Div {
    div()
        .flex_none()
        .px(px(12.0))
        .py(px(24.0))
        .text_center()
        .text_color(t.theme.text_dim)
        .font_family(SANS)
        .text_size(px(11.0))
        .line_height(px(LINE_HEIGHT))
        .child(text.into())
}

struct Tooltip {
    text: SharedString,
    tokens: Tokens,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = &self.tokens;
        div()
            .max_w(px(520.0))
            .px(px(6.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .bg(t.theme.surface_3)
            .border_1()
            .border_color(t.theme.border_2)
            .font_family(SANS)
            .text_size(px(11.0))
            .text_color(t.theme.text)
            .child(self.text.clone())
    }
}

/// The HTML `title` attribute: a hint shown while the pointer rests on the element.
pub(crate) fn tooltip(
    text: impl Into<SharedString>,
    t: &Tokens,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let (text, tokens) = (text.into(), *t);
    move |_, cx| {
        let text = text.clone();
        cx.new(|_| Tooltip { text, tokens }).into()
    }
}

/// The box of an `<input>` or `<select>`: `--surface` with a `--border-2` outline.
pub(crate) fn field_box(id: impl Into<ElementId>, t: &Tokens) -> Stateful<Div> {
    clickable(id)
        // The kit's field paints its whole line: a value longer than the box is cut here.
        .overflow_hidden()
        .bg(t.surface)
        .border_1()
        .border_color(t.theme.border_2)
        .rounded(px(4.0))
        .text_color(t.theme.text)
}

/// A closed `<select>`: the current option and the arrow. GPUI has no native popup
/// menu to hand the options to, so the panel draws [`select_menu`] under it while open.
pub(crate) fn select_box(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    t: &Tokens,
) -> Stateful<Div> {
    field_box(id, t)
        .flex()
        .items_center()
        .gap(px(4.0))
        .cursor_pointer()
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .child(
            div()
                .flex_none()
                .font_family(MONO)
                .text_size(px(9.0))
                .text_color(t.theme.text_dim)
                .child("▾"),
        )
}

/// The open list of a [`select_box`], placed under it by the caller (`absolute`). The
/// caller adds the option rows ([`select_option`]) and closes it on an outside press.
pub(crate) fn select_menu(id: impl Into<ElementId>, t: &Tokens) -> Stateful<Div> {
    clickable(id)
        .absolute()
        .left_0()
        .min_w_full()
        .py(px(4.0))
        .rounded(px(6.0))
        .bg(t.surface)
        .border_1()
        .border_color(t.theme.border_2)
        .shadow(vec![BoxShadow {
            color: Hsla::from(t.shadow),
            offset: point(px(0.0), px(4.0)),
            blur_radius: px(16.0),
            spread_radius: px(0.0),
        }])
        .text_color(t.theme.text)
        .occlude()
}

pub(crate) fn select_option(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    t: &Tokens,
) -> Stateful<Div> {
    let hover = t.theme.chrome_hover;
    clickable(id)
        .px(px(8.0))
        .py(px(3.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .whitespace_nowrap()
        .cursor_pointer()
        .when(selected, |row| row.bg(t.row_active))
        .hover(move |style| style.bg(hover))
        .child(label.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 테마에_없는_토큰은_밝은_테마와_어두운_테마를_따라간다() {
        let dark = Tokens::of(Theme::dark());
        assert_eq!(dark.row_active, rgb(0x1d2a3a));
        assert_eq!(dark.status_ok, rgb(0x6ee7b7));
        assert_eq!(dark.surface, rgb(0x1a1a1a));
        assert_eq!(dark.chip_danger_bg, rgb(0x3a1f1f));
        let light = Tokens::of(Theme::light());
        assert_eq!(light.row_active, rgb(0xe3edfa));
        assert_eq!(light.status_ok, rgb(0x1f7a34));
        assert_eq!(light.surface, rgb(0xffffff));
        assert_eq!(light.chip_danger_bg, rgb(0xfbeaea));
    }

    #[test]
    fn 밝게_한_색은_채널이_1을_넘지_않는다() {
        let color = brighten(rgb(0xffffff));
        assert_eq!((color.r, color.g, color.b), (1.0, 1.0, 1.0));
        let accent = brighten(rgb(0x4a8fff));
        assert!(accent.r > rgb(0x4a8fff).r && accent.b <= 1.0);
    }
}
