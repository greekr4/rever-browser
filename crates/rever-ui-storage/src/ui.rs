//! Pieces the panel's view is built from: the styles.css tokens the kit's theme does
//! not carry yet, the button styles, the checkbox and the `title` tooltip.

use gpui::{
    AnyView, App, Div, ElementId, FontWeight, Rgba, SharedString, Stateful, Window, div,
    prelude::*, px, rgb, rgba,
};
use rever_ui_kit::theme::{Theme, ThemeMode};

/// Stands in for the CSS `ui-monospace, monospace`.
pub(crate) const MONO: &str = "Menlo";
pub(crate) const SANS: &str = ".SystemUIFont";

/// The kit's theme plus the styles.css tokens it lacks.
#[derive(Clone, Copy)]
pub(crate) struct Tokens {
    pub theme: Theme,
    /// `--surface`
    pub surface: Rgba,
    /// `--border-3`
    pub border_3: Rgba,
    /// `--status-ok`
    pub status_ok: Rgba,
    /// `--status-info`
    pub status_info: Rgba,
    /// `--status-magenta`
    pub status_magenta: Rgba,
    /// `--chip-danger-bg`
    pub chip_danger_bg: Rgba,
    /// `--shadow`
    pub shadow: Rgba,
}

impl Tokens {
    pub fn resolve(mode: ThemeMode, window: &Window) -> Self {
        let theme = Theme::resolve(mode, window.appearance());
        // The kit decides light or dark; this only asks which one it picked.
        if theme.bg == Theme::dark().bg {
            Self {
                theme,
                surface: rgb(0x1a1a1a),
                border_3: rgb(0x444444),
                status_ok: rgb(0x6ee7b7),
                status_info: rgb(0x74b9ff),
                status_magenta: rgb(0xf0abfc),
                chip_danger_bg: rgb(0x3a1f1f),
                shadow: rgba(0x00000066),
            }
        } else {
            Self {
                theme,
                surface: rgb(0xffffff),
                border_3: rgb(0xbcc0c6),
                status_ok: rgb(0x1f7a34),
                status_info: rgb(0x1a5fb4),
                status_magenta: rgb(0x9c27b0),
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
fn brighten(color: Rgba) -> Rgba {
    Rgba {
        r: (color.r * 1.1).min(1.0),
        g: (color.g * 1.1).min(1.0),
        b: (color.b * 1.1).min(1.0),
        a: color.a,
    }
}

/// The four roles a `<button>` has in styles.css.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Role {
    /// A plain `<button>`: the secondary action.
    Plain,
    /// `.btn-ghost`: no fill until hover.
    Ghost,
    /// `.btn-primary`: the main action.
    Primary,
    /// `.btn-danger`: destructive.
    Danger,
}

/// A `<button>`. `icon` is `.btn-icon` (22px wide at least, 3px padding, centered);
/// `disabled` dims it — the caller then leaves the click handler off.
pub(crate) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    role: Role,
    icon: bool,
    disabled: bool,
    t: &Tokens,
) -> Stateful<Div> {
    let theme = t.theme;
    let (bg, color, hover_bg, hover_color) = match role {
        Role::Plain => (
            Some(theme.surface_3),
            theme.text,
            theme.border_2,
            theme.text,
        ),
        Role::Ghost => (None, theme.text_2, theme.chrome_hover, theme.text),
        Role::Primary => (
            Some(theme.accent),
            rgb(0xffffff),
            brighten(theme.accent),
            rgb(0xffffff),
        ),
        Role::Danger => (
            None,
            theme.status_error,
            t.chip_danger_bg,
            theme.status_error,
        ),
    };
    clickable(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .text_size(px(12.0))
        .line_height(px(16.0))
        .text_color(color)
        .whitespace_nowrap()
        .when_some(bg, |button, bg| button.bg(bg))
        .map(|button| {
            if icon {
                button.min_w(px(22.0)).p(px(3.0))
            } else {
                button.px(px(8.0)).py(px(3.0))
            }
        })
        .map(|button| {
            if disabled {
                // button:disabled
                button.opacity(0.42)
            } else {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover_bg).text_color(hover_color))
            }
        })
        .child(label.into())
}

/// The drawing of a native `<input type="checkbox">`, with its default margins. It
/// takes no clicks itself: the `<label>` around it does.
pub(crate) fn check_mark(checked: bool, t: &Tokens) -> Div {
    div()
        .size(px(13.0))
        .ml(px(4.0))
        .mr(px(3.0))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .border_1()
        .map(|this| {
            if checked {
                this.bg(t.theme.accent)
                    .border_color(t.theme.accent)
                    .text_color(rgb(0xffffff))
                    .text_size(px(10.0))
                    .line_height(px(11.0))
                    .font_family(SANS)
                    .font_weight(FontWeight::BOLD)
                    .child("✓")
            } else {
                this.bg(t.surface).border_color(t.border_3)
            }
        })
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
            .line_height(px(13.0))
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
