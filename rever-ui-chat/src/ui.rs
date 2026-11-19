//! Pieces the chat views share: the styles.css tokens the kit's theme does not carry
//! yet, the `title` tooltip, where a trigger was painted, and popover geometry.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyView, App, Bounds, BoxShadow, Context, Div, Entity, FontWeight, Hsla, Image, ImageFormat,
    Img, Pixels, Rgba, SharedString, TextRun, Window, canvas, div, img, point, prelude::*, px, rgb,
    rgba, transparent_black,
};
use rever_ui_kit::text_input::TextInput;
use rever_ui_kit::theme::{Theme, ThemeMode};

/// Stands in for the CSS `ui-monospace, monospace`.
pub(crate) const MONO: &str = "Menlo";

/// Stacking order of the floating pieces, as `deferred` priorities. Same order as the
/// z-index values in the components (50 for the popovers, 1000 for the prompt).
pub(crate) const Z_POPOVER: usize = 1;
pub(crate) const Z_PROMPT: usize = 5;

/// The kit's theme plus the styles.css tokens it lacks.
#[derive(Clone, Copy)]
pub(crate) struct Tokens {
    pub theme: Theme,
    pub surface: Rgba,
    pub border_3: Rgba,
    pub accent_text: Rgba,
    pub accent_soft: Rgba,
    pub accent_border: Rgba,
    pub shadow: Rgba,
    pub chip_ok_bg: Rgba,
    pub chip_ok_border: Rgba,
    pub chip_ok_text: Rgba,
    pub chip_danger_bg: Rgba,
    pub chip_danger_border: Rgba,
    pub chip_danger_text: Rgba,
    pub status_ok: Rgba,
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
                accent_text: rgb(0x4a8fff),
                accent_soft: rgba(0x508cff2e),
                accent_border: rgba(0x78aaff59),
                shadow: rgba(0x00000066),
                chip_ok_bg: rgb(0x1f2a1f),
                chip_ok_border: rgb(0x2a4a2a),
                chip_ok_text: rgb(0xaaeeaa),
                chip_danger_bg: rgb(0x3a1f1f),
                chip_danger_border: rgb(0x5a2a2a),
                chip_danger_text: rgb(0xeeaaaa),
                status_ok: rgb(0x6ee7b7),
            }
        } else {
            Self {
                theme,
                surface: rgb(0xffffff),
                border_3: rgb(0xbcc0c6),
                accent_text: rgb(0x1c56bd),
                accent_soft: rgba(0x2f6fed24),
                accent_border: rgba(0x2f6fed59),
                shadow: rgba(0x00000029),
                chip_ok_bg: rgb(0xe7f6e9),
                chip_ok_border: rgb(0xbfe3c4),
                chip_ok_text: rgb(0x1f7a34),
                chip_danger_bg: rgb(0xfbeaea),
                chip_danger_border: rgb(0xeabcbc),
                chip_danger_text: rgb(0xa52f2f),
                status_ok: rgb(0x1f7a34),
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
/// `line-height: normal` (size + 2 for 9–14px); GPUI's own default is much taller.
pub(crate) fn text(size: f32) -> Div {
    div().text_size(px(size)).line_height(px(size + 2.0))
}

/// Text as one line, the way HTML lays it out under `white-space: nowrap`: every run
/// of spaces and line breaks is one space.
pub(crate) fn single_line(text: &str) -> String {
    text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// How wide `text` (one line) is at `size` and `weight` in the font the view inherits.
pub(crate) fn natural_width(text: &str, size: f32, weight: FontWeight, window: &Window) -> f32 {
    let mut font = window.text_style().font();
    font.weight = weight;
    let run = TextRun {
        len: text.len(),
        font,
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

/// `▾`: the system font's fallback draws it as a dot at this size, so it is set in the
/// monospace font.
pub(crate) fn caret(size: f32, opacity: f32) -> Div {
    div()
        .flex_none()
        .font_family(MONO)
        .text_size(px(size))
        .opacity(opacity)
        .child("▾")
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

/// The kit's text field (20px tall, unstyled) with what it cannot draw itself laid over
/// it: a placeholder that follows the interface language, and for `masked` (an
/// `<input type="password">`) one dot per character in place of the text.
///
/// The hidden text and the dots are set in the same monospace font, so the field's own
/// cursor and selection, placed by the hidden text, line up with the dots.
// shortcut: lines up for ASCII only (a pasted key is), upgrade to a mask inside the
// kit's field if other scripts ever have to be hidden.
pub(crate) fn field(
    input: &Entity<TextInput>,
    placeholder: &'static str,
    masked: bool,
    color: Rgba,
    cx: &App,
) -> Div {
    let typed = input.read(cx).text().chars().count();
    let overlay = || div().absolute().top_0().left_0();
    div()
        .relative()
        .flex_1()
        .min_w_0()
        .h(px(20.0))
        .line_height(px(20.0))
        .text_color(color)
        .child(if masked {
            div()
                .font_family(MONO)
                .text_color(transparent_black())
                .child(input.clone())
        } else {
            div().child(input.clone())
        })
        .when(typed == 0, |field| {
            field.child(
                // A placeholder too long for its field is clipped bare, as in a browser.
                overlay()
                    .right_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .opacity(0.45)
                    .child(placeholder),
            )
        })
        .when(masked && typed > 0, |field| {
            field.child(overlay().font_family(MONO).child("•".repeat(typed)))
        })
}

/// Where an element was last painted: what `getBoundingClientRect()` gives the React
/// components when a popover opens.
pub(crate) type BoundsCell = Rc<Cell<Option<Bounds<Pixels>>>>;

/// An invisible child that fills its (relative) parent and records the parent's bounds.
/// The parent must have no padding or border of its own: an absolute child is laid out
/// inside them.
pub(crate) fn track_bounds(cell: &BoundsCell) -> impl IntoElement + use<> {
    let cell = cell.clone();
    canvas(move |bounds, _, _| cell.set(Some(bounds)), |_, _, _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

/// A box in window coordinates (`DOMRect`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl From<Bounds<Pixels>> for Rect {
    fn from(bounds: Bounds<Pixels>) -> Self {
        Self {
            left: bounds.left().into(),
            top: bounds.top().into(),
            right: bounds.right().into(),
            bottom: bounds.bottom().into(),
        }
    }
}

impl Rect {
    pub(crate) fn contains(&self, at: gpui::Point<Pixels>) -> bool {
        let (x, y) = (f32::from(at.x), f32::from(at.y));
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// The left and right edges of the box a popover has to stay inside: the chat column
/// (`closest('aside')`) or the composer (`closest('form')`). The host knows them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub left: f32,
    pub right: f32,
}

/// The value of `cubic-bezier(x1, y1, x2, y2)` at `x` (0–1).
pub fn cubic_bezier(x: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let bezier = |t: f32, p1: f32, p2: f32| {
        let u = 1.0 - t;
        3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t
    };
    let x = x.clamp(0.0, 1.0);
    // The curve's x is monotonic in t, so bisection finds the t for this x.
    let (mut low, mut high) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (low + high) / 2.0;
        if bezier(mid, x1, x2) < x {
            low = mid;
        } else {
            high = mid;
        }
    }
    bezier((low + high) / 2.0, y1, y2)
}

/// An SVG document drawn `size` pixels square.
///
/// GPUI's `svg()` element only loads from the app's asset source and tints everything
/// one color, so the views' own drawings go through the image element instead. Images
/// are cached by a hash of their bytes: building one per render is cheap.
pub(crate) fn svg_image(svg: String, size: f32) -> Img {
    img(Arc::new(Image::from_bytes(
        ImageFormat::Svg,
        svg.into_bytes(),
    )))
    .size(px(size))
    .flex_none()
}

/// The image is rasterized once, at this many pixels per CSS pixel, and scaled down
/// when drawn: enough for a 2x display with room to spare.
const RASTER_SCALE: f32 = 4.0;

/// The opening tag of an icon's SVG document, `size` CSS pixels wide.
pub(crate) fn svg_open(size: f32, attrs: &str) -> String {
    let pixels = (size * RASTER_SCALE).round().max(1.0);
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{pixels}\" height=\"{pixels}\" \
         viewBox=\"0 0 24 24\" {attrs}>"
    )
}

/// A color as an SVG paint value and its opacity.
///
/// Red and blue are written swapped. GPUI 0.2.2 decodes an in-memory SVG image
/// (`Image::to_image_data`) without the RGBA → BGRA step its other image paths have, so
/// the picture comes out with the two channels exchanged; writing them exchanged cancels
/// that.
// shortcut: relies on that GPUI behavior, upgrade (drop the swap) when a GPUI update
// makes the Claude mark come out blue instead of clay.
pub(crate) fn svg_paint(color: Rgba) -> (String, f32) {
    let byte = |channel: f32| (channel.clamp(0.0, 1.0) * 255.0).round() as u8;
    (
        format!(
            "#{:02x}{:02x}{:02x}",
            byte(color.b),
            byte(color.g),
            byte(color.r)
        ),
        color.a.clamp(0.0, 1.0),
    )
}

/// The magnifier of the model menu's search field (the component's inline SVG).
pub(crate) fn search_icon(size: f32, color: Rgba) -> Img {
    let (paint, opacity) = svg_paint(color);
    let open = svg_open(
        size,
        &format!(
            "fill=\"none\" stroke=\"{paint}\" stroke-opacity=\"{opacity}\" stroke-width=\"2\" \
             stroke-linecap=\"round\""
        ),
    );
    svg_image(
        format!("{open}<circle cx=\"11\" cy=\"11\" r=\"7\"/><path d=\"M21 21l-4.3-4.3\"/></svg>"),
        size,
    )
}

/// JavaScript's whitespace set (`String.prototype.trim`). Rust's differs by two
/// characters: U+FEFF is whitespace only in JS, U+0085 only in Rust.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// JavaScript's `Math.round`: halves go toward +∞ (`-1.5` → `-1`), where Rust's `round`
/// takes them away from zero.
pub(crate) fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// What the view tests share: a window to put one view in, and where things are in it.
#[cfg(test)]
pub(crate) mod testing {
    use std::rc::Rc;

    use gpui::{Entity, Modifiers, Pixels, Point, Render, TestAppContext, VisualTestContext};
    use rever_ui_kit::i18n::Lang;
    use rever_ui_kit::theme::ThemeMode;
    use rever_ui_menus::ui::{Look, LookFn};

    pub fn look() -> LookFn {
        Rc::new(|_| Look {
            theme_mode: ThemeMode::Light,
            lang: Lang::En,
        })
    }

    /// Open a window whose root is the view `build` makes.
    pub fn window<V: Render + 'static>(
        cx: &mut TestAppContext,
        build: impl FnOnce(&mut gpui::Window, &mut gpui::Context<V>) -> V + 'static,
    ) -> (Entity<V>, &mut VisualTestContext) {
        cx.add_window_view(build)
    }

    /// Where an element was painted. GPUI keeps these bounds after the element is gone,
    /// so a test reads them only for what is on screen.
    pub fn center(selector: &'static str, cx: &mut VisualTestContext) -> Point<Pixels> {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"))
            .center()
    }

    pub fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let at = center(selector, cx);
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 베지어_곡선은_양_끝이_고정이고_ease_in_out은_가운데가_절반이다() {
        let ease_in_out = |x| cubic_bezier(x, 0.42, 0.0, 0.58, 1.0);
        assert!(ease_in_out(0.0).abs() < 1e-3);
        assert!((ease_in_out(1.0) - 1.0).abs() < 1e-3);
        assert!((ease_in_out(0.5) - 0.5).abs() < 1e-3);
        // Slow at the start: well under the straight line.
        assert!(ease_in_out(0.2) < 0.1);
        assert!(ease_in_out(-3.0).abs() < 1e-3);
    }

    #[test]
    fn 한_줄로_만들면_줄바꿈과_이어진_공백이_한_칸이_된다() {
        assert_eq!(single_line("  a\n\n b\t\r\nc  "), "a b c");
        assert_eq!(single_line("가 나\n다"), "가 나 다");
        assert_eq!(single_line(" \n "), "");
        // A no-break space is not collapsed in HTML either.
        assert_eq!(single_line("a\u{a0} b"), "a\u{a0} b");
    }

    #[gpui::test]
    fn 글의_너비는_글이_길거나_크거나_굵을수록_넓다(
        cx: &mut gpui::TestAppContext,
    ) {
        let cx = cx.add_empty_window();
        cx.update(|window, _| {
            let width = |text: &str, size: f32, weight| natural_width(text, size, weight, window);
            assert_eq!(width("", 12.0, FontWeight::NORMAL), 0.0);
            let short = width("Opus", 12.0, FontWeight::NORMAL);
            assert!(short > 0.0);
            assert!(width("Opus Opus", 12.0, FontWeight::NORMAL) > short);
            assert!(width("Opus", 24.0, FontWeight::NORMAL) > short);
            assert!(width("Opus", 12.0, FontWeight::BOLD) >= short);
            // Whole pixels, rounded up, so the box never cuts its own text.
            assert_eq!(short, short.ceil());
        });
    }

    /// A 60px flex row holding one line that would be 400px wide.
    struct Clip;

    impl Render for Clip {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().flex().w(px(60.0)).child(
                ellipsis("a title far too long for sixty pixels", 14.0)
                    .debug_selector(|| "clip".into())
                    .w(px(400.0))
                    .min_w_0()
                    .flex_shrink(),
            )
        }
    }

    #[gpui::test]
    fn 줄임표_상자는_부모_너비까지_줄고_한_줄_높이다(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_, cx) = testing::window(cx, |_, _| Clip);
        cx.run_until_parked();
        let clip = cx.debug_bounds("clip").unwrap();
        assert_eq!(clip.size.width, px(60.0));
        assert_eq!(clip.size.height, px(14.0));
    }

    #[test]
    fn js_trim은_자바스크립트_공백만_잘라낸다() {
        assert_eq!(js_trim("\u{FEFF} a b \u{A0}\n"), "a b");
        // NEL is whitespace for Rust's trim but not for JavaScript's.
        assert_eq!(js_trim("\u{85}a\u{85}"), "\u{85}a\u{85}");
    }

    #[test]
    fn svg_색은_빨강과_파랑을_바꿔_적는다() {
        assert_eq!(svg_paint(rgb(0xd97757)), ("#5777d9".to_owned(), 1.0));
        assert_eq!(svg_paint(rgba(0x10203080)).0, "#302010");
        assert!((svg_paint(rgba(0x10203080)).1 - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn svg_문서는_네_배_크기로_그린다() {
        assert!(svg_open(16.0, "fill=\"none\"").contains("width=\"64\" height=\"64\""));
        assert!(svg_open(13.0, "").contains("width=\"52\""));
        assert!(svg_open(0.0, "").contains("width=\"1\""));
    }

    #[test]
    fn js_round는_절반을_양의_방향으로_올린다() {
        assert_eq!(js_round(-1.5), -1.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-0.4), 0.0);
        assert_eq!(js_round(-0.6), -1.0);
    }

    #[test]
    fn 사각형은_오른쪽과_아래_끝을_포함하지_않는다() {
        let rect = Rect {
            left: 10.0,
            top: 10.0,
            right: 20.0,
            bottom: 20.0,
        };
        assert!(rect.contains(point(px(10.0), px(10.0))));
        assert!(rect.contains(point(px(19.5), px(19.5))));
        assert!(!rect.contains(point(px(20.0), px(15.0))));
        assert!(!rect.contains(point(px(15.0), px(9.0))));
    }
}
