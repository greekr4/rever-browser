//! Pieces the setup views share: the styles.css tokens the kit's theme does not carry
//! yet, the `title` tooltip, the buttons of the onboarding card, a stand-in for the
//! native `<select>`, the masked field, and the providers' brand marks.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, AnyView, App, Bounds, BoxShadow, Context, Div, ElementId, Entity, Hsla, Image,
    ImageFormat, MouseButton, MouseDownEvent, Pixels, Rgba, SharedString, Stateful, Subscription,
    Window, anchored, canvas, div, img, point, prelude::*, px, rgb, rgba,
};
use rever_ui_kit::text_input::{Copy as CopyText, Cut as CutText, TextInput};
use rever_ui_kit::theme::{Theme, ThemeMode};
use rever_ui_state::constants::AcpAgentId;

pub use rever_ui_menus::ui::{Look, LookFn, fill};

/// Stands in for the CSS `ui-monospace, monospace`.
pub(crate) const MONO: &str = "Menlo";
pub(crate) const SANS: &str = ".SystemUIFont";

/// `deferred` priorities. The popups sit with the other toolbar menus (z-index 40/41 in
/// the components); the modals are above every menu (z-index 9000).
pub(crate) const Z_MENU: usize = 1;
pub(crate) const Z_MODAL: usize = 10;

/// What `button:disabled` does to any button.
pub(crate) const DISABLED_OPACITY: f32 = 0.42;

/// The kit's theme plus the styles.css tokens it lacks.
#[derive(Clone, Copy)]
pub(crate) struct Tokens {
    pub theme: Theme,
    /// `--surface`
    pub surface: Rgba,
    /// `--accent-text`
    pub accent_text: Rgba,
    /// `--accent-soft`
    pub accent_soft: Rgba,
    /// `--accent-border`
    pub accent_border: Rgba,
    /// `--status-ok`
    pub status_ok: Rgba,
    /// `--shadow`
    pub shadow: Rgba,
}

impl Tokens {
    pub fn resolve(mode: ThemeMode, window: &Window) -> Self {
        let theme = Theme::resolve(mode, window.appearance());
        // The kit decides light or dark; this only asks which one it picked.
        if theme.bg == Theme::dark().bg {
            Self::dark()
        } else {
            Self::light()
        }
    }

    pub fn light() -> Self {
        Self {
            theme: Theme::light(),
            surface: rgb(0xffffff),
            accent_text: rgb(0x1c56bd),
            // rgba(47, 111, 237, 0.14)
            accent_soft: rgba(0x2f6fed24),
            // rgba(47, 111, 237, 0.35)
            accent_border: rgba(0x2f6fed59),
            status_ok: rgb(0x1f7a34),
            // rgba(0, 0, 0, 0.16)
            shadow: rgba(0x00000029),
        }
    }

    pub fn dark() -> Self {
        Self {
            theme: Theme::dark(),
            surface: rgb(0x1a1a1a),
            accent_text: rgb(0x4a8fff),
            // rgba(80, 140, 255, 0.18)
            accent_soft: rgba(0x508cff2e),
            // rgba(120, 170, 255, 0.35)
            accent_border: rgba(0x78aaff59),
            status_ok: rgb(0x6ee7b7),
            // rgba(0, 0, 0, 0.4)
            shadow: rgba(0x00000066),
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

/// A div with an id. The view tests find it by that id; elsewhere this is `div().id(id)`.
pub(crate) fn clickable(id: impl Into<ElementId>) -> Stateful<Div> {
    let id = id.into();
    let selector = id.clone();
    div().id(id).debug_selector(move || selector.to_string())
}

/// The id `<prefix>-<name>`, for one of several like elements.
pub(crate) fn named(prefix: &str, name: &str) -> ElementId {
    SharedString::from(format!("{prefix}-{name}")).into()
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
            .font_family(SANS)
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

/// JavaScript's `String.prototype.trim`. Rust's `trim` differs by two characters:
/// U+FEFF is whitespace only in JS, U+0085 only in Rust.
pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(|c| {
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
    })
}

/// `n.toLocaleString('en-US')` for a whole number: groups of three, split by commas.
pub(crate) fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The `linkBtn` style of OnboardingModal.tsx: underlined text, no box.
pub(crate) fn link_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    disabled: bool,
    tokens: &Tokens,
) -> Stateful<Div> {
    clickable(id)
        .flex_none()
        .text_size(px(11.0))
        // The base `button` rule's line height.
        .line_height(px(16.0))
        .text_color(tokens.theme.text_2)
        .underline()
        .map(|button| {
            if disabled {
                button.opacity(DISABLED_OPACITY)
            } else {
                button.cursor_pointer()
            }
        })
        .child(label.into())
}

/// The `useBtn` style of OnboardingModal.tsx.
///
/// WCAG AA: --accent (#2f6fed) is only 4.02:1 on the light card; --accent-text clears
/// 4.5:1 in both themes.
pub(crate) fn use_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    tokens: &Tokens,
) -> Stateful<Div> {
    clickable(id)
        .flex_none()
        .px(px(14.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(tokens.accent_border)
        .bg(tokens.accent_soft)
        .text_color(tokens.accent_text)
        .text_size(px(12.0))
        .line_height(px(16.0))
        .child(label.into())
}

/// The box around a text field; the field itself draws no box.
pub(crate) fn field_box(height: f32, pad_x: f32) -> Div {
    div()
        .h(px(height))
        .px(px(pad_x))
        .flex()
        .items_center()
        .overflow_hidden()
        .border_1()
}

/// What a masked field draws for its text: one dot per character.
pub(crate) fn masked(text: &str) -> String {
    "•".repeat(text.chars().count())
}

/// `<input type="password">`: the typed text is never drawn.
///
/// The kit's field has no masked mode, so the field is drawn in a transparent color and
/// one dot per character is drawn over it. Both are in the monospace font, where a dot
/// is as wide as any character, so the caret and the selection stay where the dots are.
pub(crate) fn secret_field(
    input: &Entity<TextInput>,
    placeholder: impl Into<SharedString>,
    color: Rgba,
    cx: &App,
) -> Div {
    let dots = masked(input.read(cx).text());
    div()
        .relative()
        .flex_1()
        .min_w_0()
        .font_family(MONO)
        .text_color(gpui::transparent_black())
        // A node of its own in the key dispatch tree: without it the two listeners below
        // would land on an ancestor and block copying in every field under it.
        .key_context("SecretField")
        // As in a browser, the text of a password field cannot be copied or cut out.
        .capture_action(|_: &CopyText, _, cx| cx.stop_propagation())
        .capture_action(|_: &CutText, _, cx| cx.stop_propagation())
        .child(input.clone())
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                // The field's own line height.
                .line_height(px(20.0))
                .whitespace_nowrap()
                .map(|over| {
                    if dots.is_empty() {
                        over.font_family(SANS)
                            .text_color(color)
                            .opacity(0.45)
                            .child(placeholder.into())
                    } else {
                        over.text_color(color).child(dots)
                    }
                }),
        )
}

/// Where a closed select was last painted; its list opens under that box.
pub(crate) type Anchor = Rc<Cell<Bounds<Pixels>>>;

/// A closed `<select>`: the current option's label and the arrow. GPUI has no native
/// select; the caller styles the box and adds the click.
pub(crate) fn select_box(
    id: &'static str,
    label: impl Into<SharedString>,
    anchor: &Anchor,
    disabled: bool,
) -> Stateful<Div> {
    let anchor = anchor.clone();
    clickable(id)
        .relative()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(6.0))
        .border_1()
        .when(disabled, |select| select.opacity(DISABLED_OPACITY))
        .child(div().min_w_0().truncate().child(label.into()))
        .child(
            div()
                .flex_none()
                // The system font's fallback draws ▾ as a dot at this size.
                .font_family(MONO)
                .text_size(px(9.0))
                .opacity(0.7)
                .child("▾"),
        )
        .child(
            // Pinned to the corner (an absolute child without insets starts after the
            // padding), it fills the box inside its 1px border: put the border back.
            canvas(
                move |bounds, _, _| anchor.set(bounds.dilate(px(1.0))),
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
}

/// The open list of a select, under the box `anchor` belongs to.
///
/// Not `deferred`: the popover and the modals it opens in are deferred themselves, and
/// GPUI does not nest deferred draws. The caller adds it after everything it must cover.
#[expect(clippy::too_many_arguments, reason = "one call draws one select: the arguments are its props")]
pub(crate) fn select_list<V: 'static>(
    id: &'static str,
    options: &[SharedString],
    selected: Option<usize>,
    anchor: &Anchor,
    tokens: &Tokens,
    cx: &mut Context<V>,
    pick: fn(&mut V, usize, &mut Context<V>),
    dismiss: fn(&mut V, &mut Context<V>),
) -> AnyElement {
    let theme = tokens.theme;
    let hover = theme.chrome_hover;
    let bounds = anchor.get();
    let rows = options.iter().enumerate().map(|(index, label)| {
        clickable((id, index))
            .h(px(24.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(move |this, _, _, cx| pick(this, index, cx)))
            .child(div().flex_1().min_w_0().truncate().child(label.clone()))
            .when(selected == Some(index), |row| {
                row.child(div().flex_none().text_color(tokens.accent_text).child("✓"))
            })
            .into_any_element()
    });
    let rows: Vec<AnyElement> = rows.collect();
    anchored()
        .position(point(bounds.left(), bounds.bottom() + px(2.0)))
        .child(
            clickable(id)
                .w(bounds.size.width)
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(tokens.surface)
                .border_1()
                .border_color(theme.border_2)
                .shadow(vec![shadow(4.0, 16.0, tokens.shadow)])
                .font_family(SANS)
                .text_size(px(12.0))
                .line_height(px(16.0))
                .text_color(theme.text)
                .occlude()
                // A press anywhere else closes the list and goes no further, as with the
                // native popup.
                .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    if event.button == MouseButton::Left {
                        dismiss(this, cx);
                    }
                    cx.stop_propagation();
                }))
                .children(rows),
        )
        .into_any_element()
}

// ── Brand marks ─────────────────────────────────────────────────────────────
// What OnboardingModal.tsx draws through `chat/ProviderIcon.tsx`: Claude surfaces
// (`claude-code`, `anthropic`) → the Claude sunburst; OpenAI surfaces (`codex`,
// `openai`) → the OpenAI knot; `grok` → the Grok mark; `glm` → the Z.ai mark.
// Grok and Z.ai paths: @lobehub/icons-static-svg 1.102.0 (MIT), grok.svg / zai.svg.

const CLAUDE_CLAY: Rgba = Rgba {
    r: 0.851,
    g: 0.467,
    b: 0.341,
    a: 1.0,
};
const CLAUDE_RAY_COUNT: usize = 12;
const CLAUDE_BASE_LEN: f64 = 9.7;
const CLAUDE_HALF_WIDTH: f64 = 2.0;

const OPENAI_PATH: &str = "M22.282 9.821a5.985 5.985 0 0 0-.516-4.91 6.046 6.046 0 0 0-6.51-2.9A6.065 6.065 0 0 0 4.981 4.18a5.985 5.985 0 0 0-3.998 2.9 6.046 6.046 0 0 0 .743 7.097 5.98 5.98 0 0 0 .51 4.911 6.051 6.051 0 0 0 6.515 2.9A5.985 5.985 0 0 0 13.26 24a6.056 6.056 0 0 0 5.772-4.206 5.99 5.99 0 0 0 3.997-2.9 6.056 6.056 0 0 0-.747-7.073zM13.26 22.43a4.476 4.476 0 0 1-2.876-1.04l.141-.081 4.779-2.758a.795.795 0 0 0 .392-.681v-6.737l2.02 1.168a.071.071 0 0 1 .038.052v5.583a4.504 4.504 0 0 1-4.494 4.494zM3.6 18.304a4.47 4.47 0 0 1-.535-3.014l.142.085 4.783 2.759a.771.771 0 0 0 .78 0l5.843-3.369v2.332a.08.08 0 0 1-.033.062L9.74 19.95a4.5 4.5 0 0 1-6.14-1.646zM2.34 7.896a4.485 4.485 0 0 1 2.366-1.973V11.6a.766.766 0 0 0 .388.676l5.815 3.355-2.02 1.168a.076.076 0 0 1-.071.006l-4.83-2.786A4.504 4.504 0 0 1 2.34 7.872zm16.597 3.855l-5.833-3.387L15.119 7.15a.076.076 0 0 1 .071-.006l4.83 2.791a4.494 4.494 0 0 1-.676 8.105v-5.678a.79.79 0 0 0-.407-.667zm2.01-3.023l-.141-.085-4.774-2.782a.776.776 0 0 0-.785 0L9.409 9.23V6.897a.066.066 0 0 1 .028-.061l4.83-2.787a4.5 4.5 0 0 1 6.68 4.66zM8.3 12.863l-2.02-1.164a.08.08 0 0 1-.038-.057V6.075a4.5 4.5 0 0 1 7.375-3.453l-.142.08L8.704 5.46a.795.795 0 0 0-.393.681zm1.097-2.365l2.602-1.5 2.607 1.5v2.999l-2.597 1.5-2.607-1.5z";
const GROK_PATH: &str = "M9.27 15.29l7.978-5.897c.391-.29.95-.177 1.137.272.98 2.369.542 5.215-1.41 7.169-1.951 1.954-4.667 2.382-7.149 1.406l-2.711 1.257c3.889 2.661 8.611 2.003 11.562-.953 2.341-2.344 3.066-5.539 2.388-8.42l.006.007c-.983-4.232.242-5.924 2.75-9.383.06-.082.12-.164.179-.248l-3.301 3.305v-.01L9.267 15.292M7.623 16.723c-2.792-2.67-2.31-6.801.071-9.184 1.761-1.763 4.647-2.483 7.166-1.425l2.705-1.25a7.808 7.808 0 00-1.829-1A8.975 8.975 0 005.984 5.83c-2.533 2.536-3.33 6.436-1.962 9.764 1.022 2.487-.653 4.246-2.34 6.022-.599.63-1.199 1.259-1.682 1.925l7.62-6.815";
const GLM_PATH: &str = "M12.105 2L9.927 4.953H.653L2.83 2h9.276zM23.254 19.048L21.078 22h-9.242l2.174-2.952h9.244zM24 2L9.264 22H0L14.736 2H24z";

/// Full body near the base narrowing to a pointed tip. In f64, so the numbers print as
/// JavaScript prints them.
fn blade_path(len: f64) -> String {
    let c = 12.0;
    let tip = c - len;
    let hw = CLAUDE_HALF_WIDTH;
    format!(
        "M {c} {c} C {} {}, {} {}, {c} {tip} C {} {}, {} {}, {c} {c} Z",
        c - hw,
        c - len * 0.25,
        c - hw * 0.3,
        c - len * 0.82,
        c + hw * 0.3,
        c - len * 0.82,
        c + hw,
        c - len * 0.25,
    )
}

/// The SVG document of an agent's brand mark, or `None` when it has none. `color` fills
/// the single-color marks (`currentColor` in the component).
pub(crate) fn brand_svg(agent: AcpAgentId, color: Rgba) -> Option<String> {
    // GPUI rasterizes an in-memory SVG at its own size and then hands the pixels on as
    // BGRA without reordering them: the document is 4× the drawn size to stay crisp, and
    // its fill is written with red and blue exchanged so it comes out as intended.
    let fill = |color: Rgba| {
        let byte = |channel: f32| (channel.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!(
            "#{:02x}{:02x}{:02x}",
            byte(color.b),
            byte(color.g),
            byte(color.r)
        )
    };
    let document = |fill: String, body: String| {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"96\" height=\"96\" \
             viewBox=\"0 0 24 24\" fill=\"{fill}\">{body}</svg>"
        )
    };
    match agent {
        AcpAgentId::Anthropic | AcpAgentId::ClaudeCode => {
            // A radial burst of tapered blades in alternating long/short lengths around
            // a small solid core.
            let step = 360.0 / CLAUDE_RAY_COUNT as f64;
            let mut body: String = (0..CLAUDE_RAY_COUNT)
                .map(|i| {
                    let len = CLAUDE_BASE_LEN + if i % 2 == 0 { 1.2 } else { -1.3 };
                    format!(
                        "<path d=\"{}\" transform=\"rotate({} 12 12)\"/>",
                        blade_path(len),
                        i as f64 * step
                    )
                })
                .collect();
            body.push_str("<circle cx=\"12\" cy=\"12\" r=\"1.7\"/>");
            Some(document(fill(CLAUDE_CLAY), body))
        }
        AcpAgentId::Openai | AcpAgentId::Codex => Some(document(
            fill(color),
            format!("<path d=\"{OPENAI_PATH}\"/>"),
        )),
        AcpAgentId::Grok => Some(document(
            fill(color),
            format!("<path d=\"{GROK_PATH}\" fill-rule=\"evenodd\"/>"),
        )),
        AcpAgentId::Glm => Some(document(
            fill(color),
            format!("<path d=\"{GLM_PATH}\" fill-rule=\"evenodd\"/>"),
        )),
    }
}

/// An agent's brand mark at `size` px.
pub(crate) fn provider_icon(agent: AcpAgentId, size: f32, color: Rgba) -> Option<AnyElement> {
    let svg = brand_svg(agent, color)?;
    let image = Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()));
    Some(img(image).size(px(size)).flex_none().into_any_element())
}

/// What the view tests share: a host that records every call, and a window to put one
/// view in.
#[cfg(test)]
pub(crate) mod testing {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use gpui::{
        App, Context, Entity, Modifiers, Pixels, Point, Render, Task, TestAppContext,
        VisualTestContext, Window, div, prelude::*, px,
    };
    use rever_ui_kit::i18n::Lang;
    use rever_ui_kit::theme::ThemeMode;
    use rever_ui_state::constants::AcpAgentId;
    use rever_ui_state::tabs::{AddTabOptions, ProxyConfig};

    use super::{Look, LookFn};
    use crate::host::*;

    pub fn look() -> LookFn {
        Rc::new(|_| Look {
            theme_mode: ThemeMode::Light,
            lang: Lang::En,
        })
    }

    /// Answers with what its fields hold and writes every call down, in order.
    #[derive(Default)]
    pub struct FakeHost {
        pub calls: RefCell<Vec<String>>,
        pub health: RefCell<Vec<AgentHealth>>,
        pub browsers: RefCell<Vec<BrowserInfo>>,
        pub profiles: RefCell<Vec<BrowserProfile>>,
        pub import: RefCell<Option<Result<BrowserImportResult, String>>>,
        /// Calls (by name) that answer with this error.
        pub failing: RefCell<Vec<&'static str>>,
        pub onboarded: Cell<bool>,
        pub next_tab: Cell<u32>,
    }

    impl FakeHost {
        pub fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn log(&self, call: String) {
            self.calls.borrow_mut().push(call);
        }

        fn answer(&self, name: &'static str) -> Task<Result<(), String>> {
            Task::ready(if self.failing.borrow().contains(&name) {
                Err(format!("{name} failed"))
            } else {
                Ok(())
            })
        }
    }

    impl SetupHost for FakeHost {
        fn probe_agents(&self, _: &mut App) -> Task<Vec<AgentHealth>> {
            self.log("probe".into());
            Task::ready(self.health.borrow().clone())
        }

        fn set_api_key(
            &self,
            provider: KeyProvider,
            key: String,
            _: &mut App,
        ) -> Task<Result<(), String>> {
            self.log(format!("set_api_key {provider:?} {key}"));
            self.answer("set_api_key")
        }

        fn browsers(&self, _: &mut App) -> Task<Vec<BrowserInfo>> {
            self.log("browsers".into());
            Task::ready(self.browsers.borrow().clone())
        }

        fn browser_import(
            &self,
            opts: BrowserImportOptions,
            _: &mut App,
        ) -> Task<Result<BrowserImportResult, String>> {
            self.log(format!(
                "browser_import {}",
                serde_json::to_string(&opts).unwrap()
            ));
            Task::ready(self.import.borrow().clone().unwrap_or_else(|| {
                Ok(BrowserImportResult {
                    ok: true,
                    imported: 3,
                    total: 4,
                    ..Default::default()
                })
            }))
        }

        fn list_profiles(&self, _: &mut App) -> Task<Result<Vec<BrowserProfile>, String>> {
            self.log("list_profiles".into());
            Task::ready(Ok(self.profiles.borrow().clone()))
        }

        fn create_profile(
            &self,
            name: String,
            kind: ProfileKind,
            source: Option<String>,
            _: &mut App,
        ) -> Task<Result<BrowserProfile, String>> {
            self.log(format!("create_profile {name:?} {kind:?} {source:?}"));
            if self.failing.borrow().contains(&"create_profile") {
                return Task::ready(Err("create_profile failed".into()));
            }
            Task::ready(Ok(BrowserProfile {
                id: format!("p-{name}"),
                partition: format!("persist:rever-profile-{name}"),
                name,
                kind,
                source,
            }))
        }

        fn delete_profile(&self, id: String, _: &mut App) -> Task<Result<(), String>> {
            self.log(format!("delete_profile {id}"));
            self.answer("delete_profile")
        }

        fn register_tab_partition(
            &self,
            tab_id: String,
            partition: String,
            _: &mut App,
        ) -> Task<Result<(), String>> {
            self.log(format!("register_tab_partition {tab_id} {partition}"));
            self.answer("register_tab_partition")
        }

        fn set_proxy(
            &self,
            tab_id: String,
            config: Option<ProxyConfig>,
            partition: Option<String>,
            _: &mut App,
        ) -> Task<Result<(), String>> {
            let config = serde_json::to_string(&config).unwrap();
            // The partition of a new proxy tab is random: only whether it was passed.
            self.log(format!(
                "set_proxy {tab_id} {config} partition={}",
                partition.is_some()
            ));
            self.answer("set_proxy")
        }

        fn set_active_tab(&self, tab_id: String, _: &mut App) -> Task<Result<(), String>> {
            self.log(format!("set_active_tab {tab_id}"));
            self.answer("set_active_tab")
        }

        fn onboarded(&self, _: &App) -> bool {
            self.onboarded.get()
        }

        fn pick_agent(&self, id: AcpAgentId, path: Option<String>, _: &mut App) {
            self.onboarded.set(true);
            self.log(format!("pick_agent {} {path:?}", id.as_str()));
        }

        fn skip_onboarding(&self, _: &mut App) {
            self.onboarded.set(true);
            self.log("skip_onboarding".into());
        }

        fn add_tab(&self, url: &str, opts: AddTabOptions, _: &mut App) -> String {
            self.next_tab.set(self.next_tab.get() + 1);
            let id = format!("t{}", self.next_tab.get());
            self.log(format!(
                "add_tab {id} {url} partition={:?} profile={:?} proxy={}",
                opts.partition,
                opts.profile_id,
                opts.proxy.is_some()
            ));
            id
        }

        fn close_tab(&self, id: &str, _: &mut App) {
            self.log(format!("close_tab {id}"));
        }

        fn set_tab_proxy(&self, id: &str, proxy: Option<ProxyConfig>, _: &mut App) {
            self.log(format!("set_tab_proxy {id} {}", proxy.is_some()));
        }

        fn request_navigation(&self, url: &str, _: &mut App) {
            self.log(format!("request_navigation {url}"));
        }
    }

    /// The view in the middle of a window-filling surface that counts the mouse presses
    /// reaching it — what lies "under" a popup or a modal.
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
        cx.run_until_parked();
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

    pub fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let at = center(selector, cx);
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    }

    /// A point of the stage no view covers.
    pub fn elsewhere() -> Point<Pixels> {
        gpui::point(px(40.0), px(40.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_trim은_자바스크립트_공백만_잘라낸다() {
        assert_eq!(js_trim("\u{FEFF} sk-ant \u{A0}\n"), "sk-ant");
        // NEL is whitespace for Rust's trim but not for JavaScript's.
        assert_eq!(js_trim("\u{85}a\u{85}"), "\u{85}a\u{85}");
        assert_eq!(js_trim(" \t "), "");
    }

    #[test]
    fn 가린_입력란은_글자마다_점_하나를_그린다() {
        assert_eq!(masked(""), "");
        assert_eq!(masked("sk-ant-1"), "••••••••");
        // Per character, not per byte or UTF-16 unit.
        assert_eq!(masked("비밀😀"), "•••");
        assert!(!masked("s3cret").contains("s3cret"));
    }

    #[test]
    fn 천_단위마다_쉼표를_넣는다() {
        // (n).toLocaleString('en-US')
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(12_345), "12,345");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn 토큰은_밝은_테마와_어두운_테마의_css_값이다() {
        let (light, dark) = (Tokens::light(), Tokens::dark());
        assert_eq!(light.surface, rgb(0xffffff));
        assert_eq!(dark.surface, rgb(0x1a1a1a));
        assert_eq!(light.status_ok, rgb(0x1f7a34));
        assert_eq!(dark.status_ok, rgb(0x6ee7b7));
        assert_eq!(light.accent_text, rgb(0x1c56bd));
        // rgba(…, 0.35) and rgba(…, 0.14) as bytes.
        assert_eq!(light.accent_border, rgba(0x2f6fed59));
        assert_eq!(light.accent_soft, rgba(0x2f6fed24));
    }

    #[test]
    fn 모든_에이전트에_브랜드_마크가_있다() {
        let ink = rgb(0x1b1d21);
        for agent in rever_ui_state::constants::ACP_AGENTS {
            let svg = brand_svg(agent.id, ink).unwrap();
            assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
        }
        // The Claude mark: twelve blades and the core, in its own color whatever the ink.
        let claude = brand_svg(AcpAgentId::ClaudeCode, ink).unwrap();
        assert_eq!(claude.matches("<path").count(), 12);
        assert!(claude.contains("rotate(30 12 12)"));
        assert!(claude.contains("<circle cx=\"12\" cy=\"12\" r=\"1.7\"/>"));
        assert_eq!(
            claude,
            brand_svg(AcpAgentId::Anthropic, rgb(0xffffff)).unwrap()
        );
        // Only the lobehub marks need the even-odd rule.
        assert!(
            brand_svg(AcpAgentId::Grok, ink)
                .unwrap()
                .contains("evenodd")
        );
        assert!(
            !brand_svg(AcpAgentId::Codex, ink)
                .unwrap()
                .contains("evenodd")
        );
    }

    #[test]
    fn 날의_경로는_컴포넌트의_bladepath와_같다() {
        // bladePath(9.7 + 1.2) and bladePath(9.7 - 1.3) in ProviderIcon.tsx (node).
        assert_eq!(
            blade_path(CLAUDE_BASE_LEN + 1.2),
            "M 12 12 C 10 9.275, 11.4 3.062000000000001, 12 1.1000000000000014 \
             C 12.6 3.062000000000001, 14 9.275, 12 12 Z"
        );
        assert_eq!(
            blade_path(CLAUDE_BASE_LEN - 1.3),
            "M 12 12 C 10 9.9, 11.4 5.112000000000002, 12 3.6000000000000014 \
             C 12.6 5.112000000000002, 14 9.9, 12 12 Z"
        );
    }
}

#[cfg(test)]
mod view_tests {
    use gpui::{Entity, Focusable, TestAppContext, Window};
    use rever_ui_kit::text_input::{self, TextInput};

    use super::*;

    /// A plain field and a masked one, side by side.
    struct Fields {
        plain: Entity<TextInput>,
        secret: Entity<TextInput>,
    }

    impl Render for Fields {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(self.plain.clone())
                .child(secret_field(&self.secret, "Paste", rgb(0x000000), cx))
        }
    }

    #[gpui::test]
    fn 가린_입력란의_글자는_복사하거나_잘라낼_수_없다(cx: &mut TestAppContext) {
        cx.update(text_input::bind_keys);
        let (fields, cx) = cx.add_window_view(|_, cx| {
            let field = |text: &'static str, cx: &mut Context<Fields>| {
                cx.new(|cx| {
                    let mut input = TextInput::new("", cx);
                    input.set_text(text, cx);
                    input
                })
            };
            Fields {
                plain: field("visible", cx),
                secret: field("sk-secret", cx),
            }
        });
        let focus = |input: Entity<TextInput>, cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| window.focus(&input.focus_handle(cx)));
            cx.run_until_parked();
        };
        let (plain, secret) = fields.read_with(cx, |fields, _| {
            (fields.plain.clone(), fields.secret.clone())
        });
        let clipboard =
            |cx: &mut gpui::VisualTestContext| cx.read_from_clipboard().and_then(|c| c.text());

        // The same keys copy out of a plain field: the block is the masked field's own.
        focus(plain, cx);
        cx.simulate_keystrokes("cmd-a cmd-c");
        assert_eq!(clipboard(cx), Some("visible".to_owned()));

        focus(secret.clone(), cx);
        cx.simulate_keystrokes("cmd-a cmd-c");
        assert_eq!(clipboard(cx), Some("visible".to_owned()));
        cx.simulate_keystrokes("cmd-a cmd-x");
        assert_eq!(clipboard(cx), Some("visible".to_owned()));
        assert_eq!(
            secret.read_with(cx, |input, _| input.text().to_owned()),
            "sk-secret"
        );

        // Editing still works: select all, type over it.
        cx.simulate_keystrokes("cmd-a backspace");
        assert_eq!(secret.read_with(cx, |input, _| input.text().to_owned()), "");
    }
}
