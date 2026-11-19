//! Port of `src/renderer/src/components/PermissionPrompt.tsx` — the dialog that asks the
//! user to allow or reject a tool call the agent wants to make.
//!
//! The queue (`@/ai/acp-permission`, ported as `rever_chat_core::acp_permission`) stays
//! with the host: the prompt reads its head on every render and reports the answer as a
//! [`PermissionPromptEvent`], which the host passes on to the queue's
//! `respond_to_permission`, `approve_current_permission` or `reject_current_permission`.
//! [`ToolRules`] gives that queue the risky-tool rules this prompt shows.

use gpui::{
    App, BoxShadow, Context, EventEmitter, FocusHandle, FontWeight, Hsla, KeyDownEvent,
    Subscription, Window, anchored, deferred, div, point, prelude::*, px, rgb, rgba,
};
use rever_chat_core::RiskyTools;
use rever_chat_core::acp_permission::PermissionId;
use rever_tool_profile::risky_tools;
use rever_ui_kit::i18n::Lang;
use rever_ui_menus::ui::{LookFn, fill};
use rever_ui_state::format_json::format_output;
use serde_json::{Value, json};

use crate::ui::{MONO, Tokens, Z_PROMPT, shadow, text};

/// `src/shared/risky-tools.ts` for the permission queue: the rules this prompt shows
/// are the ones the queue decides by.
#[derive(Clone, Copy, Debug, Default)]
pub struct ToolRules;

impl RiskyTools for ToolRules {
    fn shell_http_tool(&self) -> &str {
        risky_tools::SHELL_HTTP_TOOL
    }

    fn matched_risky_tool(&self, req: &Value) -> Option<String> {
        risky_tools::matched_risky_tool(req).map(str::to_owned)
    }

    fn gated_tool_name(&self, req: &Value) -> Option<String> {
        risky_tools::gated_tool_name(req).map(str::to_owned)
    }

    fn extract_target_host(&self, raw_input: Option<&Value>) -> Option<String> {
        risky_tools::extract_target_host(raw_input?)
    }

    fn choose_allow_option(&self, req: &Value) -> String {
        risky_tools::choose_allow_option(req)
    }

    fn find_reject_option(&self, req: &Value) -> String {
        risky_tools::find_reject_option(req)
    }

    fn sanitize_selection(&self, req: &Value, option_id: &str) -> String {
        risky_tools::sanitize_selection(req, option_id)
    }
}

/// The preview is cut after this many UTF-16 units.
const PREVIEW_MAX_UNITS: usize = 600;

/// `text.slice(0, units)` when the text is longer than `units` UTF-16 code units, `None`
/// when it already fits. A surrogate pair split by the cut is left out whole, since a
/// Rust string can't hold the lone half.
fn utf16_cut(text: &str, units: usize) -> Option<&str> {
    let mut seen = 0;
    let (at, _) = text.char_indices().find(|(_, c)| {
        seen += c.len_utf16();
        seen > units
    })?;
    text.get(..at)
}

/// `JSON.stringify(value, null, 2)`.
fn stringify_pretty(value: &Value) -> String {
    // The JavaScript-exact printer of rever-ui-state is only reachable through
    // `format_output`, which unwraps strings and text blocks. An object with one plain
    // key gets neither, so the value is printed inside one and taken back out: every
    // line break in the result is the printer's own (strings escape theirs), one level
    // too deep.
    let wrapped = format_output(Some(&json!({ "v": value })));
    wrapped
        .strip_prefix("{\n  \"v\": ")
        .and_then(|rest| rest.strip_suffix("\n}"))
        .map(|inner| inner.replace("\n  ", "\n"))
        .unwrap_or(wrapped)
}

/// The tool's input as indented JSON, cut to 600 characters. `None` when the request
/// carries no input (missing or `null`).
pub fn input_preview(raw_input: Option<&Value>) -> Option<String> {
    let raw_input = raw_input.filter(|value| !value.is_null())?;
    let s = stringify_pretty(raw_input);
    Some(match utf16_cut(&s, PREVIEW_MAX_UNITS) {
        Some(head) => format!("{head}\n…"),
        None => s,
    })
}

/// One answer the request offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

impl PromptOption {
    pub fn is_allow(&self) -> bool {
        self.kind.starts_with("allow")
    }
}

/// The request's options as buttons. Gated tools can't be "always allowed" (it would
/// disable the prompt for the rest of the session), so that button is not offered for
/// them.
pub fn prompt_options(request: &Value, gated: bool) -> Vec<PromptOption> {
    let field = |option: &Value, key: &str| {
        option
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    request
        .get("options")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|option| PromptOption {
            option_id: field(option, "optionId"),
            name: field(option, "name"),
            kind: field(option, "kind"),
        })
        .filter(|option| !(gated && option.kind == "allow_always"))
        .collect()
}

/// The button that has the focus when the prompt appears: the first that allows, else
/// the first.
pub fn autofocus_index(options: &[PromptOption]) -> usize {
    options.iter().position(PromptOption::is_allow).unwrap_or(0)
}

/// The tool call's title, or the default line when it has none.
pub fn prompt_title(request: &Value, lang: Lang) -> String {
    request
        .pointer("/toolCall/title")
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| lang.t("perm.default"))
        .to_owned()
}

/// `width: min(460px, 90vw)`
pub fn card_width(viewport_width: f32) -> f32 {
    (viewport_width * 0.9).min(460.0)
}

/// What the component read from the permission stores.
#[derive(Clone, Debug, Default)]
pub struct PermissionPromptProps {
    /// The head of the queue: its id and the ACP `RequestPermissionRequest`.
    pub current: Option<(PermissionId, Value)>,
    /// How many requests are waiting, this one included.
    pub queue_len: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionPromptEvent {
    /// An option's button was pressed (`respondToPermission`).
    Respond {
        option_id: String,
        grant_session: bool,
    },
    /// Enter: the best allow option (`approveCurrentPermission`).
    Approve { grant_session: bool },
    /// Escape (`rejectCurrentPermission`).
    Reject,
}

pub struct PermissionPrompt {
    /// Read on every render and key press, as the stores are.
    props: Box<dyn Fn(&App) -> PermissionPromptProps>,
    look: LookFn,
    // "Allow all this session" checkbox — reset for every new prompt.
    grant_all: bool,
    /// The request the checkbox and the button focus belong to.
    shown: Option<PermissionId>,
    buttons: Vec<FocusHandle>,
    /// `autoFocus`: a new prompt's first allow button still has to take the focus.
    focus_pending: bool,
    _keys: Subscription,
}

impl EventEmitter<PermissionPromptEvent> for PermissionPrompt {}

impl PermissionPrompt {
    pub fn new(
        props: impl Fn(&App) -> PermissionPromptProps + 'static,
        look: LookFn,
        cx: &mut Context<Self>,
    ) -> Self {
        // Enter approves (best allow option), Escape rejects — global while a prompt
        // is showing. Covers keyboard-driven approval per the UI edge-case rules.
        // Intercepted before anything else sees the key, as the component's
        // `preventDefault` kept it from the focused field.
        let this = cx.weak_entity();
        let keys = cx.intercept_keystrokes(move |event, _, cx| {
            let handled = this
                .update(cx, |this, cx| this.on_key(&event.keystroke.key, cx))
                .unwrap_or(false);
            if handled {
                cx.stop_propagation();
            }
        });
        Self {
            props: Box::new(props),
            look,
            grant_all: false,
            shown: None,
            buttons: Vec::new(),
            focus_pending: false,
            _keys: keys,
        }
    }

    pub fn grant_all(&self) -> bool {
        self.grant_all
    }

    /// A different request at the head of the queue starts with the box unchecked.
    fn sync(&mut self, current: Option<PermissionId>) {
        if self.shown != current {
            self.shown = current;
            self.grant_all = false;
            self.focus_pending = current.is_some();
        }
    }

    fn on_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some((id, _)) = (self.props)(cx).current else {
            return false;
        };
        self.sync(Some(id));
        match key {
            "enter" => cx.emit(PermissionPromptEvent::Approve {
                grant_session: self.grant_all,
            }),
            "escape" => cx.emit(PermissionPromptEvent::Reject),
            _ => return false,
        }
        true
    }

    fn respond(&mut self, option_id: &str, cx: &mut Context<Self>) {
        cx.emit(PermissionPromptEvent::Respond {
            option_id: option_id.to_owned(),
            grant_session: self.grant_all,
        });
    }
}

/// `button:focus-visible { outline: 2px solid var(--accent) }`, as a ring around the box.
fn focus_ring(color: gpui::Rgba) -> BoxShadow {
    BoxShadow {
        color: Hsla::from(color),
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(2.0),
    }
}

impl Render for PermissionPrompt {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let props = (self.props)(cx);
        self.sync(props.current.as_ref().map(|(id, _)| *id));
        let Some((_, request)) = props.current else {
            return div().into_any_element();
        };
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let lang = look.lang;
        let viewport = window.viewport_size();

        let title = prompt_title(&request, lang);
        let raw_input = request.pointer("/toolCall/rawInput");
        let preview = input_preview(raw_input);
        let gated_tool = risky_tools::gated_tool_name(&request);
        let options = prompt_options(&request, gated_tool.is_some());
        let risky_tool = risky_tools::matched_risky_tool(&request);
        let target_host = raw_input.and_then(risky_tools::extract_target_host);

        self.buttons
            .resize_with(options.len(), || cx.focus_handle().tab_stop(true));
        if std::mem::take(&mut self.focus_pending)
            && let Some(first) = self.buttons.get(autofocus_index(&options))
        {
            window.focus(first);
        }

        let header = text(11.0)
            .flex()
            .mb(px(8.0))
            .opacity(0.55)
            // text-transform: uppercase
            .child(lang.t("perm.required").to_uppercase())
            .when(props.queue_len > 1, |header| {
                header.child(
                    div().ml(px(8.0)).opacity(0.7).child(
                        fill(
                            lang.t("perm.moreQueued"),
                            &[("n", &(props.queue_len - 1).to_string())],
                        )
                        .to_uppercase(),
                    ),
                )
            });

        let chips = (risky_tool.is_some() || target_host.is_some()).then(|| {
            text(11.0)
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.0))
                .mb(px(if preview.is_some() { 10.0 } else { 14.0 }))
                .children(risky_tool.map(|tool| {
                    div()
                        .px(px(8.0))
                        .py(px(2.0))
                        .rounded(px(5.0))
                        .border_1()
                        .border_color(tokens.chip_danger_border)
                        .bg(tokens.chip_danger_bg)
                        .text_color(tokens.chip_danger_text)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!(
                            "⚠ {}",
                            fill(lang.t("perm.riskyTool"), &[("tool", tool)])
                        ))
                }))
                .children(target_host.map(|host| {
                    div()
                        .flex()
                        .opacity(0.7)
                        .child(format!("{}:\u{a0}", lang.t("perm.target")))
                        .child(div().font_family(MONO).child(host))
                }))
        });

        let preview = preview.map(|preview| {
            // The `pre`: `max-height: 180px` of content, plus its padding and border.
            div()
                .id("permission-preview")
                .debug_selector(|| "permission-preview".into())
                .max_h(px(198.0))
                .overflow_y_scroll()
                .mb(px(14.0))
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(theme.bg)
                .border_1()
                .border_color(theme.border)
                .font_family(MONO)
                .text_size(px(11.0))
                .line_height(px(13.0))
                .child(preview)
        });

        let grant_all = self.grant_all;
        let session_grant = gated_tool.map(|tool| {
            // A `label`: a click on the box or its text flips it.
            div()
                .id("perm-session-grant")
                .debug_selector(|| "perm-session-grant".into())
                .flex()
                .items_center()
                .gap(px(6.0))
                .mb(px(12.0))
                .text_size(px(12.0))
                .line_height(px(14.0))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.grant_all = !this.grant_all;
                    cx.notify();
                }))
                .child(
                    // The native checkbox.
                    div()
                        .size(px(13.0))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded(px(3.0))
                        .text_size(px(10.0))
                        .line_height(px(10.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(0xffffff))
                        .map(|check| {
                            if grant_all {
                                check.bg(theme.accent).child("✓")
                            } else {
                                check
                                    .bg(tokens.surface)
                                    .border_1()
                                    .border_color(tokens.border_3)
                            }
                        }),
                )
                .child(fill(lang.t("perm.allowSession"), &[("tool", tool)]))
        });

        let buttons = options.iter().enumerate().map(|(index, option)| {
            let allow = option.is_allow();
            let (border, bg, color) = if allow {
                (
                    tokens.chip_ok_border,
                    tokens.chip_ok_bg,
                    tokens.chip_ok_text,
                )
            } else {
                (
                    tokens.chip_danger_border,
                    tokens.chip_danger_bg,
                    tokens.chip_danger_text,
                )
            };
            let focus = self.buttons.get(index);
            let focused = focus.is_some_and(|handle| handle.is_focused(window));
            let (click_id, key_id) = (option.option_id.clone(), option.option_id.clone());
            let kind = option.kind.clone();
            div()
                .id(("perm-option", index))
                .debug_selector(move || format!("perm-{kind}"))
                .when_some(focus, |button, handle| button.track_focus(handle))
                .px(px(14.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(border)
                .bg(bg)
                .text_color(color)
                .text_size(px(12.0))
                .line_height(px(16.0))
                .cursor_pointer()
                .when(focused, |button| {
                    button.shadow(vec![focus_ring(theme.accent)])
                })
                .on_click(cx.listener(move |this, _, _, cx| this.respond(&click_id, cx)))
                // Space presses the focused button; Enter is the prompt's own key.
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "space" {
                        this.respond(&key_id, cx);
                    }
                }))
                .child(option.name.clone())
        });

        // `card`: a content-box, so its padding and border come on top of the width.
        let card = div()
            .id("permission-card")
            .debug_selector(|| "permission-card".into())
            .w(px(card_width(viewport.width.into()) + 38.0))
            .p(px(18.0))
            .rounded(px(10.0))
            .bg(theme.bg_bar)
            .border_1()
            .border_color(theme.border_2)
            .shadow(vec![shadow(10.0, 40.0, rgba(0x00000099))])
            .text_color(theme.text)
            .text_size(px(13.0))
            .line_height(px(15.0))
            // Tab walks the buttons, as it does in a page.
            .on_key_down(|event: &KeyDownEvent, window, _| {
                if event.keystroke.key == "tab" {
                    if event.keystroke.modifiers.shift {
                        window.focus_prev();
                    } else {
                        window.focus_next();
                    }
                }
            })
            .child(header)
            .child(
                div()
                    .mb(px(8.0))
                    .text_size(px(14.0))
                    .line_height(px(17.0))
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .children(chips)
            .children(preview)
            .children(session_grant)
            .child(div().flex().flex_wrap().gap(px(8.0)).children(buttons))
            .child(
                text(11.0)
                    .mt(px(12.0))
                    .opacity(0.45)
                    .child(lang.t("perm.keys")),
            );

        // `overlay`: covers the window and takes every click.
        let overlay = div()
            .debug_selector(|| "permission-prompt".into())
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000080))
            .occlude()
            .child(card);

        deferred(anchored().position(point(px(0.0), px(0.0))).child(overlay))
            .with_priority(Z_PROMPT)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(options: Value) -> Value {
        json!({
            "sessionId": "s1",
            "toolCall": { "toolCallId": "t1", "title": "mcp__rever__browser_click" },
            "options": options
        })
    }

    fn standard() -> Value {
        json!([
            { "optionId": "always", "name": "Always Allow", "kind": "allow_always" },
            { "optionId": "once", "name": "Allow", "kind": "allow_once" },
            { "optionId": "no", "name": "Reject", "kind": "reject_once" }
        ])
    }

    #[test]
    fn 입력이_없으면_미리보기도_없다() {
        assert_eq!(input_preview(None), None);
        assert_eq!(input_preview(Some(&Value::Null)), None);
    }

    #[test]
    fn 입력은_두_칸_들여쓴_json으로_보여준다() {
        // JSON.stringify({a:1.0,b:[1,2,{c:null}],d:"x\ny",e:[],f:{}}, null, 2) in node.
        let input = json!({ "a": 1.0, "b": [1, 2, { "c": null }], "d": "x\ny", "e": [], "f": {} });
        assert_eq!(
            input_preview(Some(&input)).unwrap(),
            "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2,\n    {\n      \"c\": null\n    }\n  ],\n  \"d\": \"x\\ny\",\n  \"e\": [],\n  \"f\": {}\n}"
        );
    }

    #[test]
    fn 객체가_아닌_입력도_json으로_보여준다() {
        assert_eq!(input_preview(Some(&json!("ls -la"))).unwrap(), "\"ls -la\"");
        assert_eq!(input_preview(Some(&json!(42))).unwrap(), "42");
        assert_eq!(input_preview(Some(&json!(false))).unwrap(), "false");
        assert_eq!(input_preview(Some(&json!([]))).unwrap(), "[]");
        assert_eq!(input_preview(Some(&json!({}))).unwrap(), "{}");
        assert_eq!(input_preview(Some(&json!([1]))).unwrap(), "[\n  1\n]");
        // A text block is not unwrapped, and its line break stays escaped.
        assert_eq!(
            input_preview(Some(
                &json!({ "content": [{ "type": "text", "text": "a\n  b" }] })
            ))
            .unwrap(),
            "{\n  \"content\": [\n    {\n      \"type\": \"text\",\n      \"text\": \"a\\n  b\"\n    }\n  ]\n}"
        );
    }

    #[test]
    fn 키는_입력에_적힌_순서대로_나온다() {
        let input: Value =
            serde_json::from_str(r#"{"url":"https://a.test","method":"POST"}"#).unwrap();
        assert_eq!(
            input_preview(Some(&input)).unwrap(),
            "{\n  \"url\": \"https://a.test\",\n  \"method\": \"POST\"\n}"
        );
    }

    #[test]
    fn 숫자와_키_순서와_이스케이프는_json_stringify와_같다() {
        // JSON.stringify(JSON.parse(text), null, 2) in bun: index-like keys first, in
        // numeric order; U+2028 and U+007F stay raw.
        let input: Value = serde_json::from_str(
            "{\"a\":1e21,\"b\":1e-7,\"c\":-0.0,\"d\":123456789012345680000,\
             \"e\":0.30000000000000004,\"2\":1,\"1\":2,\"x\":\"\u{2028}\u{7f}\\u001b\"}",
        )
        .unwrap();
        assert_eq!(
            input_preview(Some(&input)).unwrap(),
            "{\n  \"1\": 2,\n  \"2\": 1,\n  \"a\": 1e+21,\n  \"b\": 1e-7,\n  \"c\": 0,\n  \
             \"d\": 123456789012345680000,\n  \"e\": 0.30000000000000004,\n  \
             \"x\": \"\u{2028}\u{7f}\\u001b\"\n}"
        );
    }

    #[test]
    fn 미리보기는_600자에서_자르고_줄임표를_붙인다() {
        // `"` + 598 letters + `"` is exactly 600 units: not cut.
        let fits = json!("a".repeat(598));
        assert_eq!(input_preview(Some(&fits)).unwrap().len(), 600);
        let long = json!("a".repeat(599));
        let preview = input_preview(Some(&long)).unwrap();
        assert_eq!(preview, format!("\"{}\n…", "a".repeat(599)));
    }

    #[test]
    fn 자르는_길이는_utf16_단위로_센다() {
        // Each of these is one UTF-16 unit but three bytes.
        let korean = json!("가".repeat(700));
        let preview = input_preview(Some(&korean)).unwrap();
        assert_eq!(preview, format!("\"{}\n…", "가".repeat(599)));
        // An emoji is two units; one split by the cut is dropped whole.
        let emoji = json!("😀".repeat(400));
        let preview = input_preview(Some(&emoji)).unwrap();
        assert_eq!(preview, format!("\"{}\n…", "😀".repeat(299)));
    }

    #[test]
    fn 선택지는_요청에_적힌_순서대로_버튼이_된다() {
        let options = prompt_options(&request(standard()), false);
        assert_eq!(
            options
                .iter()
                .map(|o| (o.option_id.as_str(), o.name.as_str(), o.kind.as_str()))
                .collect::<Vec<_>>(),
            [
                ("always", "Always Allow", "allow_always"),
                ("once", "Allow", "allow_once"),
                ("no", "Reject", "reject_once"),
            ]
        );
        assert!(options[0].is_allow() && options[1].is_allow());
        assert!(!options[2].is_allow());
    }

    #[test]
    fn 막힌_도구에는_항상_허용을_내놓지_않는다() {
        let options = prompt_options(&request(standard()), true);
        assert_eq!(
            options.iter().map(|o| o.kind.as_str()).collect::<Vec<_>>(),
            ["allow_once", "reject_once"]
        );
    }

    #[test]
    fn 선택지가_없거나_깨진_요청에서도_죽지_않는다() {
        assert!(prompt_options(&json!({}), false).is_empty());
        assert!(prompt_options(&json!({ "options": "allow" }), true).is_empty());
        let odd = prompt_options(&json!({ "options": [{ "optionId": 1 }, "x", null] }), false);
        assert_eq!(odd.len(), 3);
        assert!(odd.iter().all(|o| o.option_id.is_empty() && !o.is_allow()));
        assert_eq!(autofocus_index(&odd), 0);
        assert_eq!(autofocus_index(&[]), 0);
    }

    #[test]
    fn 처음_초점은_첫_허용_버튼에_있고_없으면_첫_버튼에_있다() {
        let reject_first = json!([
            { "optionId": "no", "name": "Reject", "kind": "reject_once" },
            { "optionId": "once", "name": "Allow", "kind": "allow_once" }
        ]);
        assert_eq!(
            autofocus_index(&prompt_options(&request(reject_first), false)),
            1
        );
        assert_eq!(
            autofocus_index(&prompt_options(&request(standard()), false)),
            0
        );
        let only_reject = json!([
            { "optionId": "no", "name": "Reject", "kind": "reject_once" },
            { "optionId": "never", "name": "Never", "kind": "reject_always" }
        ]);
        assert_eq!(
            autofocus_index(&prompt_options(&request(only_reject), false)),
            0
        );
    }

    #[test]
    fn 제목이_없거나_비었으면_기본_문구를_쓴다() {
        assert_eq!(
            prompt_title(&request(standard()), Lang::En),
            "mcp__rever__browser_click"
        );
        assert_eq!(
            prompt_title(&json!({ "toolCall": { "title": "" } }), Lang::En),
            "The agent requested permission"
        );
        assert_eq!(
            prompt_title(&json!({}), Lang::Ko),
            Lang::Ko.t("perm.default")
        );
        assert_eq!(
            prompt_title(&json!({ "toolCall": { "title": 7 } }), Lang::En),
            "The agent requested permission"
        );
    }

    #[test]
    fn 카드는_460픽셀이고_좁은_창에서는_창의_90퍼센트다() {
        assert_eq!(card_width(1000.0), 460.0);
        assert_eq!(card_width(400.0), 360.0);
    }

    #[test]
    fn 도구_규칙은_공유_규칙을_그대로_전한다() {
        let rules = ToolRules;
        let fuzz = json!({
            "toolCall": {
                "toolCallId": "t1",
                "title": "mcp__rever__intruder_run",
                "rawInput": { "url": "https://shop.test:8443/login" }
            },
            "options": standard()
        });
        assert_eq!(rules.shell_http_tool(), "shell_http");
        assert_eq!(
            rules.matched_risky_tool(&fuzz).as_deref(),
            Some("intruder_run")
        );
        assert_eq!(
            rules.gated_tool_name(&fuzz).as_deref(),
            Some("intruder_run")
        );
        assert!(rules.is_risky_tool_request(&fuzz));
        assert_eq!(
            rules.extract_target_host(fuzz.pointer("/toolCall/rawInput")),
            Some("shop.test:8443".to_owned())
        );
        assert_eq!(rules.extract_target_host(None), None);
        // "Always Allow" on a gated tool is taken down to a single allow.
        assert_eq!(rules.sanitize_selection(&fuzz, "always"), "once");
        assert_eq!(rules.choose_allow_option(&fuzz), "once");
        assert_eq!(rules.find_reject_option(&fuzz), "no");
        let click = request(standard());
        assert_eq!(rules.matched_risky_tool(&click), None);
        assert_eq!(rules.gated_tool_name(&click), None);
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc;

    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
    use rever_chat_core::acp_permission::{
        ApproveOptions, PermissionStore, RequestPermissionOutcome,
    };
    use rever_chat_core::red_team_mode::RedTeamMode;

    use super::*;
    use crate::ui::testing::{click, look, window};

    type Store = Rc<RefCell<PermissionStore<ToolRules>>>;

    struct Fixture {
        prompt: Entity<PermissionPrompt>,
        store: Store,
        events: Rc<RefCell<Vec<PermissionPromptEvent>>>,
        /// The option each answered request was resolved with.
        answers: mpsc::Receiver<String>,
        ask: mpsc::Sender<String>,
    }

    fn options() -> Value {
        json!([
            { "optionId": "always", "name": "Always Allow", "kind": "allow_always" },
            { "optionId": "once", "name": "Allow", "kind": "allow_once" },
            { "optionId": "no", "name": "Reject", "kind": "reject_once" }
        ])
    }

    fn plain(title: &str) -> Value {
        json!({
            "sessionId": "s1",
            "toolCall": { "toolCallId": "t1", "title": title, "rawInput": { "ref": "r3" } },
            "options": options()
        })
    }

    fn fuzz() -> Value {
        json!({
            "sessionId": "s1",
            "toolCall": {
                "toolCallId": "t2",
                "title": "mcp__rever__intruder_run",
                "rawInput": { "url": "https://shop.test/login", "payloads": ["a", "b"] }
            },
            "options": options()
        })
    }

    /// A prompt over a real queue; its events are passed on to the queue as the host does.
    fn prompt(cx: &mut TestAppContext) -> (Fixture, &mut VisualTestContext) {
        let mut queue = PermissionStore::new(ToolRules, |_| {});
        // Everything is asked, as with "Ask every time".
        queue.set_acp_auto_approve(false);
        let store: Store = Rc::new(RefCell::new(queue));
        let events: Rc<RefCell<Vec<PermissionPromptEvent>>> = Rc::default();
        let (seen, shown, answered) = (events.clone(), store.clone(), store.clone());
        let (prompt, cx) = window(cx, move |_, cx| {
            cx.subscribe(
                &cx.entity(),
                move |_, _, event: &PermissionPromptEvent, cx| {
                    seen.borrow_mut().push(event.clone());
                    let mut queue = answered.borrow_mut();
                    match event {
                        PermissionPromptEvent::Respond {
                            option_id,
                            grant_session,
                        } => queue.respond_to_permission(
                            option_id,
                            ApproveOptions {
                                grant_session: *grant_session,
                            },
                        ),
                        PermissionPromptEvent::Approve { grant_session } => queue
                            .approve_current_permission(ApproveOptions {
                                grant_session: *grant_session,
                            }),
                        PermissionPromptEvent::Reject => queue.reject_current_permission(),
                    }
                    cx.notify();
                },
            )
            .detach();
            PermissionPrompt::new(
                move |_| {
                    let queue = shown.borrow();
                    PermissionPromptProps {
                        current: queue
                            .current_permission()
                            .map(|pending| (pending.id(), pending.request().clone())),
                        queue_len: queue.permission_queue().len(),
                    }
                },
                look(),
                cx,
            )
        });
        let (ask, answers) = mpsc::channel();
        (
            Fixture {
                prompt,
                store,
                events,
                answers,
                ask,
            },
            cx,
        )
    }

    fn ask(fixture: &Fixture, request: Value, cx: &mut VisualTestContext) {
        let answer = fixture.ask.clone();
        fixture.store.borrow_mut().request_permission_from_user(
            request,
            &RedTeamMode::default(),
            move |response| {
                if let RequestPermissionOutcome::Selected { option_id } = response.outcome {
                    let _ = answer.send(option_id);
                }
            },
        );
        fixture.prompt.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
    }

    fn answers(fixture: &Fixture) -> Vec<String> {
        fixture.answers.try_iter().collect()
    }

    #[gpui::test]
    fn 기다리는_요청이_없으면_아무것도_그리지_않는다(cx: &mut TestAppContext) {
        let (_, cx) = prompt(cx);
        cx.run_until_parked();
        assert!(cx.debug_bounds("permission-prompt").is_none());
    }

    #[gpui::test]
    fn 요청이_오면_창을_덮고_카드를_가운데에_띄운다(cx: &mut TestAppContext) {
        let (fixture, cx) = prompt(cx);
        ask(&fixture, plain("mcp__rever__browser_click"), cx);
        let viewport = cx.update(|window, _| window.viewport_size());
        let overlay = cx.debug_bounds("permission-prompt").unwrap();
        assert_eq!(overlay.size, viewport);
        let card = cx.debug_bounds("permission-card").unwrap();
        // 460 of content, 18 of padding and a 1px border on each side.
        assert_eq!(card.size.width, px(498.0));
        assert_eq!(card.center().x, overlay.center().x);
        // Not a gated tool: all three answers, and no session checkbox.
        assert!(cx.debug_bounds("perm-allow_always").is_some());
        assert!(cx.debug_bounds("perm-allow_once").is_some());
        assert!(cx.debug_bounds("perm-reject_once").is_some());
        assert!(cx.debug_bounds("perm-session-grant").is_none());
    }

    #[gpui::test]
    fn 버튼을_누르면_그_선택지로_답한다(cx: &mut TestAppContext) {
        let (fixture, cx) = prompt(cx);
        ask(&fixture, plain("mcp__rever__browser_click"), cx);
        click("perm-reject_once", cx);
        assert_eq!(
            *fixture.events.borrow(),
            [PermissionPromptEvent::Respond {
                option_id: "no".into(),
                grant_session: false
            }]
        );
        assert_eq!(answers(&fixture), ["no"]);
    }

    #[gpui::test]
    fn 엔터는_허용하고_esc는_거절한다(cx: &mut TestAppContext) {
        let (fixture, cx) = prompt(cx);
        ask(&fixture, plain("first"), cx);
        ask(&fixture, plain("second"), cx);
        cx.simulate_keystrokes("enter");
        cx.simulate_keystrokes("escape");
        assert_eq!(
            *fixture.events.borrow(),
            [
                PermissionPromptEvent::Approve {
                    grant_session: false
                },
                PermissionPromptEvent::Reject,
            ]
        );
        // Not a gated tool, so the best allow option is "always".
        assert_eq!(answers(&fixture), ["always", "no"]);
    }

    #[gpui::test]
    fn 요청이_없을_때는_엔터와_esc를_가로채지_않는다(cx: &mut TestAppContext) {
        let (fixture, cx) = prompt(cx);
        cx.simulate_keystrokes("enter escape");
        assert!(fixture.events.borrow().is_empty());
    }

    #[gpui::test]
    fn 막힌_도구는_항상_허용이_없고_세션_허용을_고를_수_있다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = prompt(cx);
        ask(&fixture, fuzz(), cx);
        // A second call of the same tool waits behind it.
        ask(&fixture, fuzz(), cx);
        assert!(cx.debug_bounds("perm-allow_once").is_some());
        assert!(cx.debug_bounds("perm-session-grant").is_some());
        click("perm-session-grant", cx);
        assert!(fixture.prompt.read_with(cx, |prompt, _| prompt.grant_all()));
        click("perm-allow_once", cx);
        assert_eq!(
            *fixture.events.borrow(),
            [PermissionPromptEvent::Respond {
                option_id: "once".into(),
                grant_session: true
            }]
        );
        // The grant covers the waiting call too.
        assert_eq!(answers(&fixture), ["once", "once"]);
        assert!(fixture.store.borrow().current_permission().is_none());
    }

    #[gpui::test]
    fn 세션_허용_체크는_다음_요청에서_다시_꺼져_있다(cx: &mut TestAppContext) {
        let (fixture, cx) = prompt(cx);
        ask(&fixture, fuzz(), cx);
        ask(&fixture, plain("mcp__rever__replay_request"), cx);
        click("perm-session-grant", cx);
        click("perm-session-grant", cx);
        click("perm-session-grant", cx);
        assert!(fixture.prompt.read_with(cx, |prompt, _| prompt.grant_all()));
        click("perm-reject_once", cx);
        cx.run_until_parked();
        assert!(!fixture.prompt.read_with(cx, |prompt, _| prompt.grant_all()));
        // Enter right away answers the next request without the old check mark.
        cx.simulate_keystrokes("enter");
        assert_eq!(
            fixture.events.borrow().last(),
            Some(&PermissionPromptEvent::Approve {
                grant_session: false
            })
        );
    }

    #[gpui::test]
    fn 처음_초점은_첫_허용_버튼에_있고_스페이스로_누른다(
        cx: &mut TestAppContext,
    ) {
        let (fixture, cx) = prompt(cx);
        ask(&fixture, fuzz(), cx);
        cx.simulate_keystrokes("space");
        assert_eq!(
            *fixture.events.borrow(),
            [PermissionPromptEvent::Respond {
                option_id: "once".into(),
                grant_session: false
            }]
        );
    }

    #[gpui::test]
    fn 프롬프트_밖을_눌러도_닫히지_않는다(cx: &mut TestAppContext) {
        let (fixture, cx) = prompt(cx);
        ask(&fixture, plain("mcp__rever__browser_click"), cx);
        cx.simulate_click(point(px(5.0), px(5.0)), Modifiers::none());
        cx.run_until_parked();
        assert!(fixture.events.borrow().is_empty());
        assert!(fixture.store.borrow().current_permission().is_some());
    }
}
