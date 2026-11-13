//! Pieces the panel's views share: the color tokens the kit's theme does not
//! carry yet, the button styles of styles.css, the checkbox and the tooltip.

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
    pub surface: Rgba,
    pub border_3: Rgba,
    pub accent_text: Rgba,
    pub accent_soft: Rgba,
    pub accent_border: Rgba,
    pub glass_panel: Rgba,
    pub glass_hairline: Rgba,
    pub shadow: Rgba,
    pub chip_ok_bg: Rgba,
    pub chip_ok_border: Rgba,
    pub row_active: Rgba,
    pub row_selected: Rgba,
    pub json_key: Rgba,
    pub json_string: Rgba,
    pub json_number: Rgba,
    pub json_bool: Rgba,
    pub json_null: Rgba,
    pub json_punct: Rgba,
}

impl Tokens {
    pub fn resolve(mode: ThemeMode, window: &Window) -> Self {
        let theme = Theme::resolve(mode, window.appearance());
        // The kit decides light or dark; this only asks which one it picked.
        if theme.bg == Theme::dark().bg {
            Self::dark(theme)
        } else {
            Self::light(theme)
        }
    }

    fn dark(theme: Theme) -> Self {
        Self {
            theme,
            surface: rgb(0x1a1a1a),
            border_3: rgb(0x444444),
            accent_text: rgb(0x4a8fff),
            accent_soft: rgba(0x508cff2e),
            accent_border: rgba(0x78aaff59),
            glass_panel: rgba(0x141416d9),
            glass_hairline: rgba(0xffffff14),
            shadow: rgba(0x00000066),
            chip_ok_bg: rgb(0x1f2a1f),
            chip_ok_border: rgb(0x2a4a2a),
            row_active: rgb(0x1d2a3a),
            row_selected: rgb(0x1a1f1a),
            json_key: rgb(0x79c0ff),
            json_string: rgb(0xa5d6a7),
            json_number: rgb(0xffb86c),
            json_bool: rgb(0xd8a0ff),
            json_null: rgb(0x909090),
            json_punct: rgb(0x808080),
        }
    }

    fn light(theme: Theme) -> Self {
        Self {
            theme,
            surface: rgb(0xffffff),
            border_3: rgb(0xbcc0c6),
            accent_text: rgb(0x1c56bd),
            accent_soft: rgba(0x2f6fed24),
            accent_border: rgba(0x2f6fed59),
            glass_panel: rgba(0xf9fafbe6),
            glass_hairline: rgba(0x0000001a),
            shadow: rgba(0x00000029),
            chip_ok_bg: rgb(0xe7f6e9),
            chip_ok_border: rgb(0xbfe3c4),
            row_active: rgb(0xe3edfa),
            row_selected: rgb(0xeaf4ec),
            json_key: rgb(0x0a58ca),
            json_string: rgb(0x1f7a34),
            json_number: rgb(0xa45c00),
            json_bool: rgb(0x7b3fb5),
            json_null: rgb(0x61676e),
            json_punct: rgb(0x61676e),
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

fn button_base(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    clickable(id)
        .flex_none()
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(4.0))
        .text_size(px(12.0))
        .line_height(px(16.0))
        .cursor_pointer()
        .child(label.into())
}

/// A plain `<button>`: the secondary action.
pub(crate) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    t: &Tokens,
) -> Stateful<Div> {
    let hover = t.theme.border_2;
    button_base(id, label)
        .bg(t.theme.surface_3)
        .text_color(t.theme.text)
        .hover(move |style| style.bg(hover))
}

/// `.btn-ghost`: no fill until hover.
pub(crate) fn ghost_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    t: &Tokens,
) -> Stateful<Div> {
    let (hover, text) = (t.theme.chrome_hover, t.theme.text);
    button_base(id, label)
        .text_color(t.theme.text_2)
        .hover(move |style| style.bg(hover).text_color(text))
}

/// `.btn-primary`: the main action.
pub(crate) fn primary_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    t: &Tokens,
) -> Stateful<Div> {
    let hover = brighten(t.theme.accent);
    button_base(id, label)
        .bg(t.theme.accent)
        .text_color(rgb(0xffffff))
        .hover(move |style| style.bg(hover))
}

/// `.seg > button`: one of a group, the pressed one underlined.
pub(crate) fn seg_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    pressed: bool,
    t: &Tokens,
) -> Stateful<Div> {
    let text = t.theme.text;
    clickable(id)
        .px(px(6.0))
        .pt(px(3.0))
        .pb(px(1.0))
        .border_b_2()
        .border_color(if pressed {
            t.theme.accent
        } else {
            rgba(0x00000000)
        })
        .text_size(px(12.0))
        .line_height(px(16.0))
        .text_color(if pressed { text } else { t.theme.text_dim })
        .cursor_pointer()
        .hover(move |style| style.text_color(text))
        .child(label.into())
}

/// Stands in for the native `<input type="checkbox">`.
pub(crate) fn checkbox(id: impl Into<ElementId>, checked: bool, t: &Tokens) -> Stateful<Div> {
    clickable(id)
        .size(px(13.0))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .border_1()
        .cursor_pointer()
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
