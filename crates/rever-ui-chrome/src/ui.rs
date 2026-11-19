//! Pieces the chrome widgets share: the styles.css tokens the kit's theme does not
//! carry yet, the `title` tooltip and the browser's text metrics.

use std::rc::Rc;

use gpui::{
    AnyView, App, BoxShadow, Context, Div, ElementId, Hsla, Rgba, SharedString, Stateful, TextRun,
    Window, div, font, point, prelude::*, px, rgb, rgba,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};

/// The theme and language a view renders with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Look {
    pub theme_mode: ThemeMode,
    pub lang: Lang,
}

/// Read on every render, as `useT()` and the theme attribute are in the React app, so a
/// language or theme switch shows without the host pushing it into each view.
pub type LookFn = Rc<dyn Fn(&App) -> Look>;

/// The kit's theme plus the styles.css tokens it lacks.
#[derive(Clone, Copy)]
pub(crate) struct Tokens {
    pub theme: Theme,
    pub surface: Rgba,
    pub accent_soft: Rgba,
    pub accent_border: Rgba,
    pub proxy_tab_bg: Rgba,
    pub proxy_tab_text: Rgba,
    pub proxy_tab_dot: Rgba,
}

impl Tokens {
    pub fn resolve(mode: ThemeMode, window: &Window) -> Self {
        let theme = Theme::resolve(mode, window.appearance());
        // The kit decides light or dark; this only asks which one it picked.
        if theme.bg == Theme::dark().bg {
            Self {
                theme,
                surface: rgb(0x1a1a1a),
                accent_soft: rgba(0x508cff2e),
                accent_border: rgba(0x78aaff59),
                proxy_tab_bg: rgb(0x241a2e),
                proxy_tab_text: rgb(0xc9aaee),
                proxy_tab_dot: rgb(0xa06fe0),
            }
        } else {
            Self {
                theme,
                surface: rgb(0xffffff),
                accent_soft: rgba(0x2f6fed24),
                accent_border: rgba(0x2f6fed59),
                proxy_tab_bg: rgb(0xf4edfc),
                proxy_tab_text: rgb(0x6b3fa8),
                proxy_tab_dot: rgb(0x8a55d4),
            }
        }
    }
}

/// `box-shadow: 0 <y>px <blur>px <color>`.
pub(crate) fn shadow(y: f32, blur: f32, color: Rgba) -> BoxShadow {
    BoxShadow {
        color: Hsla::from(color),
        offset: point(px(0.0), px(y)),
        blur_radius: px(blur),
        spread_radius: px(0.0),
    }
}

/// Text at `size` with the line height a browser gives the system font at
/// `line-height: normal` (size + 2 for 9–13px); GPUI's own default is much taller.
pub(crate) fn text(size: f32) -> Div {
    div().text_size(px(size)).line_height(px(size + 2.0))
}

/// The interface font: `-apple-system` and its kin in the stylesheet.
pub(crate) const SANS: &str = ".SystemUIFont";

/// Text as one line, the way HTML lays it out under `white-space: nowrap`: every run
/// of spaces and line breaks is one space. A page's title can hold line breaks, and
/// GPUI's line shaping does not accept them.
pub(crate) fn single_line(text: &str) -> String {
    text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// How wide `text` (already [`single_line`]) is at `size` in the interface font.
pub(crate) fn natural_width(text: &str, size: f32, window: &Window) -> f32 {
    let run = TextRun {
        len: text.len(),
        font: font(SANS),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(text.to_owned().into(), px(size), &[run], None);
    f32::from(line.width).ceil()
}

/// One line (already [`single_line`]) `line` pixels tall, cut with an ellipsis at the
/// width of the box this returns; the caller gives that box its width.
///
/// GPUI shapes an unwrapped text once, at its first measurement, and a flex item's
/// first measurement is for its content size: the text would never be cut. An
/// absolutely placed box is measured only when its parent is laid out for good.
///
/// That holds as long as every box around it, up to one of a set size, is a flex box:
/// the layout engine sizes a block of unknown height by laying its children out, at
/// whatever width it is trying, and tries every flex item at its narrowest.
pub(crate) fn ellipsis(text: impl Into<SharedString>, line: f32) -> Div {
    div().relative().h(px(line)).child(
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .truncate()
            .child(text.into()),
    )
}

/// A div with an id. The view tests find it by that id; elsewhere this is `div().id(id)`.
pub(crate) fn clickable(id: impl Into<ElementId>) -> Stateful<Div> {
    let id = id.into();
    let selector = id.clone();
    div().id(id).debug_selector(move || selector.to_string())
}

struct Tooltip {
    text: SharedString,
    tokens: Tokens,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        text(11.0)
            .max_w(px(360.0))
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(self.tokens.theme.border_2)
            .bg(self.tokens.theme.surface_3)
            .text_color(self.tokens.theme.text)
            .font_family(".SystemUIFont")
            .child(self.text.clone())
    }
}

/// The HTML `title` attribute: GPUI has no native tooltip, so this is a small box of its own.
pub(crate) fn tooltip(
    text: impl Into<SharedString>,
    tokens: &Tokens,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let (text, tokens) = (text.into(), *tokens);
    move |_, cx| {
        let text = text.clone();
        cx.new(|_| Tooltip { text, tokens }).into()
    }
}

/// What the view tests share: a look, and where things are in the window.
#[cfg(test)]
pub(crate) mod testing {
    use std::rc::Rc;

    use gpui::{Pixels, Point, VisualTestContext};
    use rever_ui_kit::i18n::Lang;
    use rever_ui_kit::theme::ThemeMode;

    use super::{Look, LookFn};

    pub fn look() -> LookFn {
        Rc::new(|_| Look {
            theme_mode: ThemeMode::Light,
            lang: Lang::En,
        })
    }

    /// Where an element was painted. GPUI keeps these bounds after the element is gone,
    /// so a test reads them only for what is on screen, and asks the view's state
    /// whether something has disappeared.
    pub fn center(selector: &'static str, cx: &mut VisualTestContext) -> Point<Pixels> {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"))
            .center()
    }
}
