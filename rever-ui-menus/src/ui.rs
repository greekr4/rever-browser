//! Pieces the widgets share: the styles.css tokens the kit's theme does not carry yet,
//! the `title` tooltip, `{name}` placeholders and CSS animation timing.

use std::rc::Rc;

use gpui::{
    AnyView, App, BoxShadow, Context, Div, Hsla, Rgba, SharedString, Subscription, Window, div,
    point, prelude::*, px, rgb, rgba,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};

/// Stands in for the CSS `ui-monospace, monospace`.
pub(crate) const MONO: &str = "Menlo";

/// Stacking order of the floating pieces, as `deferred` priorities. Same order as the
/// z-index values in styles.css (50, 100, 9998, 9999).
pub(crate) const Z_MENU: usize = 1;
pub(crate) const Z_BOTCHECK: usize = 2;
pub(crate) const Z_COPY_BOX: usize = 3;
pub(crate) const Z_TOAST: usize = 4;

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
    pub accent_text: Rgba,
    pub glass_panel: Rgba,
    pub glass_hairline: Rgba,
    pub glass_hover: Rgba,
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
                accent_text: rgb(0x4a8fff),
                glass_panel: rgba(0x141416d9),
                glass_hairline: rgba(0xffffff14),
                glass_hover: rgba(0xffffff0f),
                shadow: rgba(0x00000066),
            }
        } else {
            Self {
                theme,
                surface: rgb(0xffffff),
                accent_text: rgb(0x1c56bd),
                glass_panel: rgba(0xf9fafbe6),
                glass_hairline: rgba(0x0000001a),
                glass_hover: rgba(0x0000000d),
                shadow: rgba(0x00000029),
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

/// Window-level, so Esc works even after focus left the popup. Keep the subscription
/// only while the popup is open.
pub(crate) fn on_escape<T: 'static>(
    cx: &mut Context<T>,
    close: impl Fn(&mut T, &mut Context<T>) + 'static,
) -> Subscription {
    cx.observe_keystrokes(move |this, event, _, cx| {
        if event.keystroke.key == "escape" {
            close(this, cx);
        }
    })
}

/// `t(key, vars)` of `src/renderer/src/stores/i18n.ts`: every `{name}` is replaced, one
/// variable after the other, so a value that itself holds a later placeholder is filled too.
pub fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (name, value) in vars {
        out = replace_all(&out, &format!("{{{name}}}"), value);
    }
    out
}

/// JavaScript's `text.replaceAll(pattern, replacement)` with a string pattern. The
/// replacement is not taken literally there: `$$` is one `$`, `$&` the match, `` $` ``
/// and `$'` the text before and after the match. A file name or a page's dialog
/// message with a `$` in it goes through this, so the notices read the same in both apps.
fn replace_all(text: &str, pattern: &str, replacement: &str) -> String {
    if pattern.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (at, matched) in text.match_indices(pattern) {
        let after = at + matched.len();
        out.push_str(text.get(copied..at).unwrap_or_default());
        let mut rest = replacement;
        while let Some(dollar) = rest.find('$') {
            let (literal, tail) = rest.split_at(dollar);
            out.push_str(literal);
            let (insert, used) = match tail.as_bytes().get(1) {
                Some(b'$') => ("$", 2),
                Some(b'&') => (matched, 2),
                Some(b'`') => (text.get(..at).unwrap_or_default(), 2),
                Some(b'\'') => (text.get(after..).unwrap_or_default(), 2),
                // `$1` and `$<name>` need a regex's groups; with a string pattern they,
                // and a `$` before anything else, stay as written.
                _ => ("$", 1),
            };
            out.push_str(insert);
            rest = tail.get(used..).unwrap_or_default();
        }
        out.push_str(rest);
        copied = after;
    }
    out.push_str(text.get(copied..).unwrap_or_default());
    out
}

/// The CSS `ease` timing function, `cubic-bezier(0.25, 0.1, 0.25, 1)`.
pub fn css_ease(x: f32) -> f32 {
    let bezier = |t: f32, p1: f32, p2: f32| {
        let u = 1.0 - t;
        3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t
    };
    let x = x.clamp(0.0, 1.0);
    // The curve's x is monotonic in t, so bisection finds the t for this x.
    let (mut low, mut high) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (low + high) / 2.0;
        if bezier(mid, 0.25, 0.25) < x {
            low = mid;
        } else {
            high = mid;
        }
    }
    bezier((low + high) / 2.0, 0.1, 1.0)
}

/// The value of a CSS `@keyframes` property at `progress` (0–1) of an `ease` animation.
/// `stops` are (offset, value) pairs in rising offset order; as in CSS the timing
/// function runs inside each pair of stops, not across the whole animation.
pub fn keyframe(stops: &[(f32, f32)], progress: f32) -> f32 {
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
        return 0.0;
    };
    if progress <= first.0 {
        return first.1;
    }
    for pair in stops.windows(2) {
        let [(from_at, from), (to_at, to)] = pair else {
            continue;
        };
        if progress <= *to_at {
            let span = to_at - from_at;
            if span <= 0.0 {
                return *to;
            }
            return from + (to - from) * css_ease((progress - from_at) / span);
        }
    }
    last.1
}

/// What the view tests share: a window to put one widget in, and where things are in it.
#[cfg(test)]
pub(crate) mod testing {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{
        Context, Entity, Pixels, Point, Render, TestAppContext, VisualTestContext, Window, div,
        prelude::*, px,
    };
    use rever_ui_kit::i18n::Lang;
    use rever_ui_kit::theme::ThemeMode;

    use super::{Look, LookFn};

    pub fn look() -> LookFn {
        Rc::new(|_| Look {
            theme_mode: ThemeMode::Light,
            lang: Lang::En,
        })
    }

    /// The widget in the middle of a window-filling surface that counts the mouse
    /// presses reaching it — what lies "under" a popup.
    pub struct Stage<V> {
        child: Entity<V>,
        presses: Rc<Cell<usize>>,
    }

    impl<V: Render> Render for Stage<V> {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let presses = self.presses.clone();
            div()
                .size_full()
                .child(
                    div()
                        .absolute()
                        .size_full()
                        .on_any_mouse_down(move |_, _, _| presses.set(presses.get() + 1)),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(600.0))
                        .top(px(400.0))
                        .w(px(200.0))
                        .h(px(40.0))
                        .flex()
                        .child(self.child.clone()),
                )
        }
    }

    /// Open a window on a [`Stage`] holding the view `build` makes. Returns the view,
    /// the stage's press counter and the window's test context.
    pub fn stage<V: Render + 'static>(
        cx: &mut TestAppContext,
        build: impl FnOnce(&mut Context<V>) -> V + 'static,
    ) -> (Entity<V>, Rc<Cell<usize>>, &mut VisualTestContext) {
        let presses = Rc::new(Cell::new(0));
        let (stage, cx) = cx.add_window_view({
            let presses = presses.clone();
            move |_, cx| Stage {
                child: cx.new(build),
                presses,
            }
        });
        let child = stage.read_with(cx, |stage, _| stage.child.clone());
        (child, presses, cx)
    }

    /// Where an element was painted. GPUI keeps these bounds after the element is gone,
    /// so a test reads them only for what is on screen, and asks the view's state
    /// whether something has disappeared.
    pub fn center(selector: &'static str, cx: &mut VisualTestContext) -> Point<Pixels> {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"))
            .center()
    }

    /// A point of the stage no widget covers.
    pub fn elsewhere() -> Point<Pixels> {
        gpui::point(px(40.0), px(40.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 자리표시자를_값으로_채운다() {
        assert_eq!(
            fill(
                "Download {state}: {name}",
                &[("name", "a.zip"), ("state", "cancelled")]
            ),
            "Download cancelled: a.zip"
        );
    }

    #[test]
    fn 같은_자리표시자가_여러_번_나오면_모두_채운다() {
        assert_eq!(fill("{n} of {n}", &[("n", "3")]), "3 of 3");
    }

    #[test]
    fn 모르는_자리표시자는_그대로_둔다() {
        assert_eq!(fill("Hello {who}", &[("name", "x")]), "Hello {who}");
        assert_eq!(fill("Hello {who}", &[]), "Hello {who}");
    }

    #[test]
    fn 앞_값에_들어_있는_뒤_자리표시자도_순서대로_채워진다() {
        // replaceAll runs once per variable, in order, over the whole string.
        assert_eq!(
            fill("{type}", &[("type", "{answer}"), ("answer", "OK")]),
            "OK"
        );
        assert_eq!(
            fill("{type}", &[("answer", "OK"), ("type", "{answer}")]),
            "{answer}"
        );
    }

    #[test]
    fn 값_안의_달러_기호는_자바스크립트_replaceall처럼_풀린다() {
        // Expected strings are what the TypeScript `t()` returns for the same input (bun).
        let name = |value| fill("Downloaded {name}", &[("name", value)]);
        assert_eq!(name("a$$b.zip"), "Downloaded a$b.zip");
        assert_eq!(name("a$&b"), "Downloaded a{name}b");
        assert_eq!(fill("x {name} y", &[("name", "[$`|$']")]), "x [x | y] y");
        assert_eq!(
            fill("x {name} y", &[("name", "$1 $0 $<n> $")]),
            "x $1 $0 $<n> $ y"
        );
        assert_eq!(fill("{a}", &[("a", "$$$")]), "$$");
        assert_eq!(fill("{a}", &[("a", "$$$$")]), "$$");
        assert_eq!(fill("{a}", &[("a", "가$$나😀$")]), "가$나😀$");
    }

    #[test]
    fn 달러_기호의_앞뒤_문맥은_그_변수를_채우기_전의_문자열이다() {
        assert_eq!(fill("{n} of {n}", &[("n", "$&")]), "{n} of {n}");
        assert_eq!(fill("{n} and {n}", &[("n", "<$`>")]), "<> and <{n} and >");
        assert_eq!(fill("{n} and {n}", &[("n", "<$'>")]), "< and {n}> and <>");
        assert_eq!(
            fill("{type} {answer}", &[("type", "{answer}"), ("answer", "$&")]),
            "{answer} {answer}"
        );
    }

    #[test]
    fn ease_곡선은_양_끝이_고정이고_중간은_css_값과_같다() {
        assert!(css_ease(0.0).abs() < 1e-3);
        assert!((css_ease(1.0) - 1.0).abs() < 1e-3);
        // cubic-bezier(0.25, 0.1, 0.25, 1) at x = 0.5.
        assert!((css_ease(0.5) - 0.8024).abs() < 2e-3);
        assert!(css_ease(-1.0).abs() < 1e-3);
        assert!((css_ease(2.0) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn 키프레임은_구간마다_ease로_보간한다() {
        let stops = [(0.0, 0.0), (0.12, 1.0), (0.75, 1.0), (1.0, 0.0)];
        assert_eq!(keyframe(&stops, 0.0), 0.0);
        assert!((keyframe(&stops, 0.06) - css_ease(0.5)).abs() < 1e-4);
        assert_eq!(keyframe(&stops, 0.5), 1.0);
        assert!((keyframe(&stops, 0.875) - (1.0 - css_ease(0.5))).abs() < 1e-4);
        assert!(keyframe(&stops, 1.0).abs() < 1e-3);
    }

    #[test]
    fn 키프레임_범위_밖과_빈_목록에서도_값을_돌려준다() {
        assert_eq!(keyframe(&[], 0.5), 0.0);
        assert_eq!(keyframe(&[(0.0, 2.0), (1.0, 4.0)], -1.0), 2.0);
        assert_eq!(keyframe(&[(0.0, 2.0), (1.0, 4.0)], 3.0), 4.0);
        assert_eq!(keyframe(&[(0.5, 2.0), (0.5, 4.0)], 0.5), 2.0);
    }
}
