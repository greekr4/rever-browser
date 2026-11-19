//! Pieces the panel and its editors share: the styles.css tokens the kit's theme does
//! not carry yet, the button classes, and stand-ins for the native form controls
//! (`<input>`, `<textarea>`, `<select>`, checkbox, `title` tooltip).

use std::rc::Rc;

use gpui::{
    AnyView, App, BoxShadow, Context, Div, ElementId, Entity, FontWeight, Hsla, MouseButton,
    MouseDownEvent, Rgba, SharedString, Stateful, Window, deferred, div, point, prelude::*, px,
    relative, rgb, rgba,
};
use rever_ui_kit::text_input::TextInput;
use rever_ui_kit::theme::{Theme, ThemeMode};

use crate::text_area::TextArea;

/// Stands in for the CSS `ui-monospace, monospace`.
pub(crate) const MONO: &str = "Menlo";
pub(crate) const SANS: &str = ".SystemUIFont";

/// Chromium's default font size for `<input>`, `<select>` and `<textarea>`, which do not
/// inherit the panel's 12px.
pub(crate) const CONTROL_TEXT: f32 = 13.333;

/// `deferred` priorities: an open select's list, then the delete dialog above everything.
pub(crate) const Z_SELECT: usize = 1;
pub(crate) const Z_DIALOG: usize = 2;

/// The kit's theme plus the styles.css tokens it lacks.
#[derive(Clone, Copy)]
pub(crate) struct Tokens {
    pub theme: Theme,
    /// `--surface`
    pub surface: Rgba,
    /// `--border-3`
    pub border_3: Rgba,
    /// `--code-bg`
    pub code_bg: Rgba,
    /// `--accent-text`
    pub accent_text: Rgba,
    /// `--accent-soft`
    pub accent_soft: Rgba,
    /// `--chip-ok-text`
    pub chip_ok_text: Rgba,
    /// `--chip-ok-border`
    pub chip_ok_border: Rgba,
    /// `--chip-warn-border`
    pub chip_warn_border: Rgba,
    /// `--chip-danger-bg`
    pub chip_danger_bg: Rgba,
    /// `--shadow`
    pub shadow: Rgba,
    /// The `#e06c6c` the components write inline for errors, in both themes.
    pub error: Rgba,
}

impl Tokens {
    pub fn resolve(mode: ThemeMode, window: &Window) -> Self {
        let theme = Theme::resolve(mode, window.appearance());
        let error = rgb(0xe06c6c);
        // The kit decides light or dark; this only asks which one it picked.
        if theme.bg == Theme::dark().bg {
            Self {
                theme,
                surface: rgb(0x1a1a1a),
                border_3: rgb(0x444444),
                code_bg: rgb(0x0c0c0c),
                accent_text: rgb(0x4a8fff),
                accent_soft: rgba(0x508cff2e),
                chip_ok_text: rgb(0xaaeeaa),
                chip_ok_border: rgb(0x2a4a2a),
                chip_warn_border: rgb(0x4a402a),
                chip_danger_bg: rgb(0x3a1f1f),
                shadow: rgba(0x00000066),
                error,
            }
        } else {
            Self {
                theme,
                surface: rgb(0xffffff),
                border_3: rgb(0xbcc0c6),
                code_bg: rgb(0xf3f4f6),
                accent_text: rgb(0x1c56bd),
                accent_soft: rgba(0x2f6fed24),
                chip_ok_text: rgb(0x1f7a34),
                chip_ok_border: rgb(0xbfe3c4),
                chip_warn_border: rgb(0xe6d5a8),
                chip_danger_bg: rgb(0xfbeaea),
                shadow: rgba(0x00000029),
                error,
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

/// `text.replaceAll(pattern, replacement)` for a string pattern. JavaScript expands
/// `$$`, `$&`, `` $` `` and `$'` in the replacement even then; `$1` and `$<name>` stay
/// as written, since a string pattern captures nothing.
fn replace_all_js(text: &str, pattern: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, found) in text.match_indices(pattern) {
        let end = start + found.len();
        out.push_str(text.get(last..start).unwrap_or_default());
        let mut chars = replacement.chars().peekable();
        while let Some(c) = chars.next() {
            let expanded = match (c, chars.peek()) {
                ('$', Some('$')) => "$",
                ('$', Some('&')) => found,
                ('$', Some('`')) => text.get(..start).unwrap_or_default(),
                ('$', Some('\'')) => text.get(end..).unwrap_or_default(),
                _ => {
                    out.push(c);
                    continue;
                }
            };
            out.push_str(expanded);
            chars.next();
        }
        last = end;
    }
    out.push_str(text.get(last..).unwrap_or_default());
    out
}

/// `t(key, vars)` of `src/renderer/src/stores/i18n.ts`: every `{name}` is replaced,
/// one variable after the other.
pub(crate) fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (name, value) in vars {
        out = replace_all_js(&out, &format!("{{{name}}}"), value);
    }
    out
}

/// Text for a one-line slot. `white-space: nowrap` lays line breaks out as spaces;
/// GPUI would start a new line.
pub(crate) fn single_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// Text as a normal-flow HTML element shows it: runs of spaces and line breaks
/// collapse to one space (`white-space: normal`).
pub(crate) fn html_text(text: &str) -> String {
    let is_space = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}');
    text.split(is_space)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A `<label>` stacking its caption over its control: column, 3px gap, dim text.
pub(crate) fn field_label(t: &Tokens) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(3.0))
        .text_color(t.theme.text_dim)
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

/// The button classes of styles.css the workflows use.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ButtonKind {
    /// A plain `<button>`: the secondary action.
    Plain,
    /// `.btn-primary`
    Primary,
    /// `.btn-ghost`
    Ghost,
    /// `.btn-danger`
    Danger,
    /// `.btn-toggle` with its `aria-pressed`.
    Toggle(bool),
}

/// A `<button>`. A disabled one is dimmed and takes no hover; the caller attaches
/// `on_click` only when `enabled`.
pub(crate) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
    t: &Tokens,
) -> Stateful<Div> {
    let theme = t.theme;
    let (bg, color, hover_bg, hover_color) = match kind {
        ButtonKind::Plain | ButtonKind::Toggle(false) => (
            Some(theme.surface_3),
            theme.text,
            theme.border_2,
            theme.text,
        ),
        ButtonKind::Toggle(true) => (
            Some(t.accent_soft),
            t.accent_text,
            theme.border_2,
            t.accent_text,
        ),
        ButtonKind::Primary => (
            Some(theme.accent),
            rgb(0xffffff),
            brighten(theme.accent),
            rgb(0xffffff),
        ),
        ButtonKind::Ghost => (None, theme.text_2, theme.chrome_hover, theme.text),
        ButtonKind::Danger => (
            None,
            theme.status_error,
            t.chip_danger_bg,
            theme.status_error,
        ),
    };
    clickable(id)
        .flex_none()
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(4.0))
        .text_size(px(12.0))
        .line_height(px(16.0))
        .font_family(SANS)
        .text_color(color)
        .when_some(bg, |button, bg| button.bg(bg))
        .map(|button| {
            if enabled {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover_bg).text_color(hover_color))
            } else {
                // button:disabled
                button.opacity(0.42)
            }
        })
        .child(label.into())
}

/// The box of an `<input>`: the base `input` rule plus the inline height and padding.
pub(crate) fn input_box(input: &Entity<TextInput>, height: f32, pad_x: f32, t: &Tokens) -> Div {
    div()
        .h(px(height))
        .px(px(pad_x))
        .flex()
        .items_center()
        .overflow_hidden()
        .bg(t.surface)
        .text_color(t.theme.text)
        .border_1()
        .border_color(t.theme.border_2)
        .rounded(px(4.0))
        .font_family(SANS)
        .text_size(px(CONTROL_TEXT))
        .child(input.clone())
}

/// The box of a `<textarea rows={rows}>` in the monospace style the editors share.
/// The field scrolls inside it; `padding` is the field's own (see [`TextArea::padding`]).
pub(crate) fn area_box(
    area: &Entity<TextArea>,
    rows: usize,
    font_size: f32,
    padding: f32,
    bg: Rgba,
    t: &Tokens,
) -> Div {
    // `line-height: normal` of the monospace font.
    let line = (font_size * 1.2).round();
    div()
        .h(px(rows as f32 * line + 2.0 * padding + 2.0))
        .flex_none()
        .overflow_hidden()
        .bg(bg)
        .text_color(t.theme.text)
        .border_1()
        .border_color(t.theme.border_2)
        .rounded(px(4.0))
        .font_family(MONO)
        .text_size(px(font_size))
        .line_height(px(line))
        .child(area.clone())
}

/// A text field holding `text`. Created in one go so the observer the caller adds next
/// first fires on a real edit.
pub(crate) fn new_input(
    text: &str,
    placeholder: impl Into<SharedString>,
    cx: &mut App,
) -> Entity<TextInput> {
    let (text, placeholder) = (text.to_owned(), placeholder.into());
    cx.new(|cx| {
        let mut input = TextInput::new(placeholder, cx);
        input.set_text(text, cx);
        input
    })
}

/// A multi-line field holding `text`, with the textarea's `padding`.
pub(crate) fn new_area(
    text: &str,
    placeholder: impl Into<SharedString>,
    padding: f32,
    cx: &mut App,
) -> Entity<TextArea> {
    let (text, placeholder) = (text.to_owned(), placeholder.into());
    cx.new(|cx| {
        let mut area = TextArea::new(placeholder, cx).padding(padding);
        area.set_text(text, cx);
        area
    })
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
        text(11.0)
            .max_w(px(360.0))
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .bg(t.theme.surface_3)
            .border_1()
            .border_color(t.theme.border_2)
            .font_family(SANS)
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

/// One `<option>`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SelectOption {
    pub value: SharedString,
    pub label: SharedString,
}

impl SelectOption {
    pub fn new(value: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }

    /// An option whose label is its value.
    pub fn plain(value: impl Into<SharedString>) -> Self {
        let value = value.into();
        Self {
            label: value.clone(),
            value,
        }
    }
}

/// The closed select shows the chosen option's label, or the raw value when no option
/// has it (a tool that is no longer offered stays readable).
pub(crate) fn select_label(options: &[SelectOption], value: &str) -> SharedString {
    options
        .iter()
        .find(|option| option.value.as_ref() == value)
        .map_or_else(|| value.to_owned().into(), |option| option.label.clone())
}

/// Stands in for `<select>`: a box showing the current option that opens a list under
/// itself. The view keeps which select is open (`open`), toggles it in `on_toggle`, and
/// gets the picked value in `on_pick` (the list is already closed by then).
pub(crate) fn select<V: 'static>(
    key: impl Into<SharedString>,
    value: &str,
    options: Vec<SelectOption>,
    open: bool,
    height: f32,
    t: &Tokens,
    cx: &mut Context<V>,
    on_toggle: impl Fn(&mut V, bool, &mut Context<V>) + 'static,
    on_pick: impl Fn(&mut V, &str, &mut Context<V>) + 'static,
) -> Div {
    let key: SharedString = key.into();
    let theme = t.theme;
    let hover = theme.chrome_hover;
    let on_toggle = Rc::new(on_toggle);
    let on_pick = Rc::new(on_pick);

    let closed = clickable(ElementId::Name(key.clone()))
        .h(px(height))
        .w_full()
        .px(px(6.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .overflow_hidden()
        .bg(t.surface)
        .text_color(theme.text)
        .border_1()
        .border_color(theme.border_2)
        .rounded(px(4.0))
        .font_family(SANS)
        .text_size(px(CONTROL_TEXT))
        .line_height(px(16.0))
        .cursor_pointer()
        .on_click(cx.listener({
            let on_toggle = on_toggle.clone();
            move |this, _, _, cx| on_toggle(this, !open, cx)
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(select_label(&options, value)),
        )
        .child(
            div()
                .flex_none()
                // The system font's fallback draws ▾ as a dot at this size.
                .font_family(MONO)
                .text_size(px(9.0))
                .text_color(theme.text_dim)
                .child("▾"),
        );

    let wrap = div().relative().min_w_0().child(closed);
    if !open {
        return wrap;
    }

    let mut rows = Vec::with_capacity(options.len());
    for (index, option) in options.into_iter().enumerate() {
        let chosen = option.value.as_ref() == value;
        let picked = option.value.clone();
        let (on_toggle, on_pick) = (on_toggle.clone(), on_pick.clone());
        let row = clickable(ElementId::Name(format!("{key}-option-{index}").into()))
            .w_full()
            .px(px(8.0))
            .py(px(3.0))
            .whitespace_nowrap()
            .cursor_pointer()
            .when(chosen, |row| row.bg(t.accent_soft))
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(move |this, _, _, cx| {
                on_toggle(this, false, cx);
                on_pick(this, &picked, cx);
            }))
            .child(option.label);
        rows.push(row);
    }

    let list = div()
        .id(ElementId::Name(format!("{key}-list").into()))
        .absolute()
        .top(relative(1.0))
        .mt(px(2.0))
        .left_0()
        .min_w_full()
        .max_h(px(240.0))
        .overflow_y_scroll()
        .py(px(4.0))
        .rounded(px(6.0))
        .bg(t.surface)
        .border_1()
        .border_color(theme.border_2)
        .shadow(vec![shadow(4.0, 16.0, t.shadow)])
        .font_family(SANS)
        .text_size(px(CONTROL_TEXT))
        .line_height(px(18.0))
        .text_color(theme.text)
        .occlude()
        // A click outside closes the list and goes no further, as with a native select.
        .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
            if event.button == MouseButton::Left {
                on_toggle(this, false, cx);
            }
            cx.stop_propagation();
        }))
        .children(rows);

    wrap.child(deferred(list).with_priority(Z_SELECT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 자리표시자를_값으로_채운다() {
        assert_eq!(fill("Matched {count}", &[("count", "3")]), "Matched 3");
        assert_eq!(
            fill(
                "{limit} from {selector}",
                &[("limit", "5"), ("selector", ".item")]
            ),
            "5 from .item"
        );
        assert_eq!(fill("{n} of {n}", &[("n", "2")]), "2 of 2");
        assert_eq!(fill("Hello {who}", &[]), "Hello {who}");
    }

    // Expected values read from String.prototype.replaceAll under bun.
    #[test]
    fn 값에_든_달러_패턴은_replaceall처럼_펼친다() {
        assert_eq!(
            fill("x {selector} y", &[("selector", "$& $$ $` $1 $<a> $")]),
            "x {selector} $ x  $1 $<a> $ y"
        );
        assert_eq!(fill("{n} of {n}!", &[("n", "$'")]), " of {n}! of !!");
        // 흔한 CSS 선택자의 $= 는 패턴이 아니다.
        assert_eq!(
            fill(
                "Returns up to {limit} items: {selector}",
                &[("limit", "5"), ("selector", "a[href$='.pdf']")]
            ),
            "Returns up to 5 items: a[href$='.pdf']"
        );
    }

    #[test]
    fn 일반_흐름의_글자는_공백과_줄바꿈을_한_칸으로_줄인다() {
        assert_eq!(html_text("  a \n\n b\t c  "), "a b c");
        // 줄바꿈 없는 공백(U+00A0)은 줄이지 않는다.
        assert_eq!(html_text("a\u{a0}\u{a0}b"), "a\u{a0}\u{a0}b");
        assert_eq!(html_text(""), "");
    }

    #[test]
    fn 한_줄_자리의_줄바꿈은_공백이_된다() {
        assert_eq!(single_line("a\nb\r\nc"), "a b  c");
    }

    #[test]
    fn 고른_값의_라벨을_보여주고_없는_값은_그대로_보여준다() {
        let options = vec![
            SelectOption::new("__ai_prompt", "__ai_prompt — ask the AI"),
            SelectOption::plain("browser_click"),
        ];
        assert_eq!(
            select_label(&options, "__ai_prompt").as_ref(),
            "__ai_prompt — ask the AI"
        );
        assert_eq!(
            select_label(&options, "browser_click").as_ref(),
            "browser_click"
        );
        assert_eq!(select_label(&options, "gone_tool").as_ref(), "gone_tool");
        assert_eq!(select_label(&[], "").as_ref(), "");
    }
}
