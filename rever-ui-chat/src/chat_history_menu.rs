//! Port of `src/renderer/src/components/chat/ChatHistoryMenu.tsx` — the dropdown listing
//! saved conversations, newest first. Clicking a row opens it; the × removes it.
//!
//! The list comes from the host on every render (the `useChatHistory` store there), and
//! opening or removing a conversation is reported as a [`ChatHistoryEvent`]: the store's
//! `remove` needs the host's storage.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    App, Context, EventEmitter, FontWeight, MouseDownEvent, Window, anchored, deferred, div, point,
    prelude::*, px, rgba,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_menus::ui::{LookFn, fill};
use rever_ui_state::chat_history::ChatConversation;
use rever_ui_state::constants::{ACP_AGENTS, AcpAgentId};

use crate::provider_icon::provider_icon;
use crate::ui::{
    BoundsCell, Rect, Span, Tokens, Z_POPOVER, caret, ellipsis, js_round, shadow, single_line,
    tooltip, track_bounds,
};

/// What the menu shows of a saved conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRow {
    pub id: String,
    pub title: String,
    pub agent_id: AcpAgentId,
    /// Milliseconds since the epoch.
    pub updated_at: u64,
}

impl From<&ChatConversation> for HistoryRow {
    fn from(conversation: &ChatConversation) -> Self {
        Self {
            id: conversation.id.clone(),
            title: conversation.title.clone(),
            agent_id: conversation.agent_id,
            updated_at: conversation.updated_at,
        }
    }
}

/// What the React component takes as props, plus the store's list.
#[derive(Clone, Debug, Default)]
pub struct ChatHistoryMenuProps {
    pub conversations: Vec<HistoryRow>,
    pub current_id: Option<String>,
    pub disabled: bool,
    /// The chat column the popover has to stay inside; the whole window when `None`.
    pub column: Option<Span>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatHistoryEvent {
    /// A row was clicked: open that conversation.
    Open(String),
    /// A row's × was clicked: remove that conversation from the store.
    Remove(String),
}

#[derive(Clone, Copy)]
enum Unit {
    Minute,
    Hour,
    Day,
    Week,
}

/// `n.toLocaleString()` for a whole number: thousands separated by commas (the same in
/// English and Korean).
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `new Intl.RelativeTimeFormat(lang, { numeric: 'auto', style: 'short' }).format(n, unit)`
/// for the values `relative_time` produces (any minute count but 0, and past hours,
/// days and weeks).
fn format_relative(n: f64, unit: Unit, lang: Lang) -> String {
    let past = n < 0.0;
    let count = n.abs() as u64;
    let number = grouped(count);
    match lang {
        Lang::En => {
            let word = match (unit, count) {
                (Unit::Day, 1) if past => return "yesterday".to_owned(),
                (Unit::Week, 1) if past => return "last wk.".to_owned(),
                (Unit::Minute, _) => "min.",
                (Unit::Hour, _) => "hr.",
                (Unit::Day, 1) => "day",
                (Unit::Day, _) => "days",
                (Unit::Week, _) => "wk.",
            };
            if past {
                format!("{number} {word} ago")
            } else {
                format!("in {number} {word}")
            }
        }
        Lang::Ko => {
            let word = match (unit, count) {
                (Unit::Day, 1) if past => return "어제".to_owned(),
                (Unit::Day, 2) if past => return "그저께".to_owned(),
                (Unit::Week, 1) if past => return "지난주".to_owned(),
                (Unit::Minute, _) => "분",
                (Unit::Hour, _) => "시간",
                (Unit::Day, _) => "일",
                (Unit::Week, _) => "주",
            };
            format!("{number}{word} {}", if past { "전" } else { "후" })
        }
    }
}

/// How long ago `ts_ms` was, seen from `now_ms`, in the interface language.
pub fn relative_time(ts_ms: u64, now_ms: u64, lang: Lang) -> String {
    let min = js_round((ts_ms as f64 - now_ms as f64) / 60_000.0);
    if min == 0.0 {
        // "now" — 0 minutes reads oddly in Korean
        return match lang {
            Lang::En => "now",
            Lang::Ko => "지금",
        }
        .to_owned();
    }
    if min > -60.0 {
        return format_relative(min, Unit::Minute, lang);
    }
    let hr = js_round(min / 60.0);
    if hr > -24.0 {
        return format_relative(hr, Unit::Hour, lang);
    }
    let day = js_round(hr / 24.0);
    if day > -7.0 {
        format_relative(day, Unit::Day, lang)
    } else {
        format_relative(js_round(day / 7.0), Unit::Week, lang)
    }
}

/// Where the popover goes, in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub top: f32,
    pub left: f32,
    pub width: f32,
}

/// Fixed and clamped to the chat column: an absolute popover hanging left of the button
/// got clipped. The popover's right edge lines up with the button's.
pub fn popover_placement(trigger: Rect, column: Option<Span>, viewport_width: f32) -> Placement {
    let col = column.unwrap_or(Span {
        left: 0.0,
        right: viewport_width,
    });
    let width = (col.right - col.left - 16.0).min(320.0);
    let left = (col.left + 8.0).max((trigger.right - width).min(col.right - width - 8.0));
    Placement {
        top: trigger.bottom + 6.0,
        left,
        width,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

pub struct ChatHistoryMenu {
    /// Read on every render, as re-rendered props and the store are.
    props: Box<dyn Fn(&App) -> ChatHistoryMenuProps>,
    look: LookFn,
    open: bool,
    trigger: BoundsCell,
}

impl EventEmitter<ChatHistoryEvent> for ChatHistoryMenu {}

impl ChatHistoryMenu {
    pub fn new(props: impl Fn(&App) -> ChatHistoryMenuProps + 'static, look: LookFn) -> Self {
        Self {
            props: Box::new(props),
            look,
            open: false,
            trigger: BoundsCell::default(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.open = open;
        cx.notify();
    }
}

impl Render for ChatHistoryMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let lang = look.lang;
        let props = (self.props)(cx);
        let count = props.conversations.len().to_string();
        let hover = theme.chrome_hover;

        // `button.btn-ghost`
        let trigger = div()
            .id("chat-history-trigger")
            .debug_selector(|| "chat-history-trigger".into())
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.0))
            .px(px(8.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .text_size(px(12.0))
            .line_height(px(16.0))
            .text_color(theme.text_2)
            .whitespace_nowrap()
            .when(props.disabled, |button| button.opacity(0.42))
            .when(!props.disabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover).text_color(theme.text))
                    .on_click(cx.listener(|this, _, _, cx| this.set_open(!this.open, cx)))
            })
            .tooltip(tooltip(lang.t("chat.historyTitle"), &tokens))
            .child(lang.t("chat.history"))
            .child(
                div()
                    .opacity(0.6)
                    .text_size(px(10.0))
                    .child(format!("({count})")),
            )
            .child(caret(10.0, 0.6));

        let wrap = div()
            .relative()
            .flex()
            .flex_none()
            .child(track_bounds(&self.trigger))
            .child(trigger);
        if !self.open {
            return wrap;
        }
        // The button is painted before a click can open the menu; only a menu opened
        // from code on the very first frame has to wait for the button's place.
        let Some(trigger) = self.trigger.get().map(Rect::from) else {
            window.request_animation_frame();
            return wrap;
        };
        let viewport = window.viewport_size();
        let place = popover_placement(trigger, props.column, viewport.width.into());
        let now = now_ms();

        // `popoverHeader`
        let header = div()
            .flex()
            .flex_none()
            .justify_between()
            .items_center()
            .pt(px(2.0))
            .px(px(4.0))
            .pb(px(8.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .line_height(px(14.0))
                    .font_weight(FontWeight::BOLD)
                    .child(lang.t("chat.history")),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .line_height(px(13.0))
                    .opacity(0.6)
                    .child(fill(lang.t("chat.historySaved"), &[("n", &count)])),
            );

        let rows = props.conversations.iter().enumerate().map(|(index, c)| {
            let agent = ACP_AGENTS.iter().find(|agent| agent.id == c.agent_id);
            let current = props.current_id.as_deref() == Some(c.id.as_str());
            let (open_id, remove_id) = (c.id.clone(), c.id.clone());
            // `rowStyle`
            div()
                .flex()
                .flex_none()
                .bg(theme.bg_bar)
                .border_1()
                .border_color(if current { theme.accent } else { theme.border })
                .rounded(px(6.0))
                .overflow_hidden()
                .child(
                    // `rowMainStyle`: a `button`, so its lines are 16px tall.
                    div()
                        .id(("chat-history-open", index))
                        .debug_selector(move || format!("chat-history-open-{index}"))
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(8.0))
                        .py(px(6.0))
                        .text_size(px(12.0))
                        .line_height(px(16.0))
                        .cursor_pointer()
                        .tooltip(tooltip(c.title.clone(), &tokens))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.emit(ChatHistoryEvent::Open(open_id.clone()));
                            this.set_open(false, cx);
                        }))
                        .child(
                            // `iconChip`
                            div()
                                .id(("chat-history-agent", index))
                                .size(px(20.0))
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.0))
                                .bg(theme.surface_2)
                                .when_some(agent, |chip, agent| {
                                    chip.tooltip(tooltip(agent.name, &tokens))
                                })
                                .child(provider_icon(
                                    c.agent_id.as_str(),
                                    14.0,
                                    Some(agent.map_or("?", |agent| agent.icon)),
                                    theme.text,
                                )),
                        )
                        .child(
                            // `rowTextStyle`
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(ellipsis(single_line(&c.title), 16.0))
                                .child(
                                    div().text_size(px(10.0)).opacity(0.5).child(relative_time(
                                        c.updated_at,
                                        now,
                                        lang,
                                    )),
                                ),
                        ),
                )
                .child(
                    // `align-self: center; margin-right: 4px` of the × button.
                    div().flex().flex_none().items_center().mr(px(4.0)).child(
                        // `button.btn-danger.btn-icon`
                        div()
                            .id(("chat-history-delete", index))
                            .debug_selector(move || format!("chat-history-delete-{index}"))
                            .min_w(px(22.0))
                            .p(px(3.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.0))
                            .text_size(px(15.0))
                            .line_height(px(16.0))
                            .text_color(theme.status_error)
                            .cursor_pointer()
                            .hover(move |style| style.bg(tokens.chip_danger_bg))
                            .tooltip(tooltip(lang.t("chat.deleteConversation"), &tokens))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(ChatHistoryEvent::Remove(remove_id.clone()));
                            }))
                            .child("×"),
                    ),
                )
        });

        // `popoverStyle`
        let popover = div()
            .id("chat-history-popover")
            .debug_selector(|| "chat-history-popover".into())
            .w(px(place.width))
            .max_h(viewport.height * 0.7)
            // A flex column, not a block: see `ellipsis`.
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .p(px(10.0))
            .rounded(px(8.0))
            .bg(theme.bg_bar)
            .border_1()
            .border_color(theme.border)
            .shadow(vec![shadow(8.0, 24.0, rgba(0x00000080))])
            .text_color(theme.text)
            .occlude()
            // A press anywhere else closes the menu and still reaches what is under it,
            // as with the component's document listener. A press on the button is left
            // to the button, which toggles.
            .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                if !trigger.contains(event.position) {
                    this.set_open(false, cx);
                }
            }))
            .child(header)
            .map(|popover| {
                if props.conversations.is_empty() {
                    popover.child(
                        div()
                            .flex_none()
                            .px(px(4.0))
                            .py(px(8.0))
                            .text_size(px(11.0))
                            .line_height(px(13.0))
                            .opacity(0.5)
                            // Not in the dictionaries: the component has it inline.
                            .child("No saved conversations yet."),
                    )
                } else {
                    // `listStyle`
                    popover.child(
                        div()
                            .flex()
                            .flex_none()
                            .flex_col()
                            .gap(px(4.0))
                            .children(rows),
                    )
                }
            });

        wrap.child(
            deferred(
                anchored()
                    .snap_to_window()
                    .position(point(px(place.left), px(place.top)))
                    .child(popover),
            )
            .with_priority(Z_POPOVER),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60_000;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    const NOW: u64 = 1_800_000_000_000;

    fn ago(ms: u64, lang: Lang) -> String {
        relative_time(NOW - ms, NOW, lang)
    }

    #[test]
    fn 삼십_초_안쪽은_지금이다() {
        assert_eq!(ago(0, Lang::En), "now");
        assert_eq!(ago(29_999, Lang::En), "now");
        assert_eq!(ago(20_000, Lang::Ko), "지금");
        // Math.round takes -0.5 to -0, so exactly half a minute is still "now".
        assert_eq!(ago(30_000, Lang::En), "now");
        assert_eq!(ago(30_001, Lang::En), "1 min. ago");
    }

    #[test]
    fn 한_시간_안쪽은_분으로_말한다() {
        assert_eq!(ago(MIN, Lang::En), "1 min. ago");
        assert_eq!(ago(5 * MIN, Lang::En), "5 min. ago");
        assert_eq!(ago(59 * MIN, Lang::En), "59 min. ago");
        assert_eq!(ago(5 * MIN, Lang::Ko), "5분 전");
    }

    #[test]
    fn 하루_안쪽은_시간으로_말한다() {
        assert_eq!(ago(60 * MIN, Lang::En), "1 hr. ago");
        // 89.5 minutes rounds to -89, and -89 / 60 to -1.
        assert_eq!(ago(89 * MIN + 30_000, Lang::En), "1 hr. ago");
        // -90 / 60 = -1.5, which Math.round takes up to -1.
        assert_eq!(ago(90 * MIN, Lang::En), "1 hr. ago");
        assert_eq!(ago(91 * MIN, Lang::En), "2 hr. ago");
        assert_eq!(ago(23 * HOUR, Lang::En), "23 hr. ago");
        assert_eq!(ago(3 * HOUR, Lang::Ko), "3시간 전");
    }

    #[test]
    fn 일주일_안쪽은_날로_말하고_하루_전은_어제다() {
        assert_eq!(ago(24 * HOUR, Lang::En), "yesterday");
        assert_eq!(ago(24 * HOUR, Lang::Ko), "어제");
        assert_eq!(ago(2 * DAY, Lang::En), "2 days ago");
        assert_eq!(ago(2 * DAY, Lang::Ko), "그저께");
        assert_eq!(ago(6 * DAY, Lang::En), "6 days ago");
        assert_eq!(ago(6 * DAY, Lang::Ko), "6일 전");
        // Math.round takes -23.5 hours up to -23; a minute more is a day.
        assert_eq!(ago(23 * HOUR + 30 * MIN, Lang::En), "23 hr. ago");
        assert_eq!(ago(23 * HOUR + 31 * MIN, Lang::En), "yesterday");
    }

    #[test]
    fn 일주일부터는_주로_말한다() {
        assert_eq!(ago(7 * DAY, Lang::En), "last wk.");
        assert_eq!(ago(7 * DAY, Lang::Ko), "지난주");
        assert_eq!(ago(14 * DAY, Lang::En), "2 wk. ago");
        assert_eq!(ago(14 * DAY, Lang::Ko), "2주 전");
        assert_eq!(ago(364 * DAY, Lang::En), "52 wk. ago");
    }

    #[test]
    fn 큰_수는_쉼표로_끊는다() {
        assert_eq!(ago(1234 * 7 * DAY, Lang::En), "1,234 wk. ago");
        assert_eq!(ago(1234 * 7 * DAY, Lang::Ko), "1,234주 전");
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(1234567), "1,234,567");
    }

    #[test]
    fn 앞선_시각은_몇_분_후라고_말한다() {
        // A clock that moved back: the component has no branch for it, minutes it is.
        assert_eq!(relative_time(NOW + 5 * MIN, NOW, Lang::En), "in 5 min.");
        assert_eq!(relative_time(NOW + 5 * MIN, NOW, Lang::Ko), "5분 후");
        assert_eq!(
            relative_time(NOW + 1234 * MIN, NOW, Lang::En),
            "in 1,234 min."
        );
        assert_eq!(relative_time(NOW + 20_000, NOW, Lang::En), "now");
    }

    #[test]
    fn 단위가_바뀌는_경계는_자바스크립트의_반올림을_따른다() {
        // relativeTime() of the component, run with bun on the same offsets.
        for (ms, en, ko) in [
            (59 * MIN + 30_000, "59 min. ago", "59분 전"),
            (59 * MIN + 30_001, "1 hr. ago", "1시간 전"),
            (36 * HOUR, "yesterday", "어제"),
            (37 * HOUR, "2 days ago", "그저께"),
            (60 * HOUR, "2 days ago", "그저께"),
            (61 * HOUR, "3 days ago", "3일 전"),
            (6 * DAY + 12 * HOUR, "6 days ago", "6일 전"),
            (6 * DAY + 13 * HOUR, "last wk.", "지난주"),
            (10 * DAY + 12 * HOUR, "last wk.", "지난주"),
            (11 * DAY, "2 wk. ago", "2주 전"),
        ] {
            assert_eq!(ago(ms, Lang::En), en, "{ms}");
            assert_eq!(ago(ms, Lang::Ko), ko, "{ms}");
        }
        // Half a minute ahead rounds up to a whole one; days ahead stay minutes.
        assert_eq!(relative_time(NOW + 30_000, NOW, Lang::En), "in 1 min.");
        assert_eq!(relative_time(NOW + 3 * DAY, NOW, Lang::Ko), "4,320분 후");
    }

    #[test]
    fn 아주_오래된_시각도_주로_말한다() {
        // Date.now() - 0: the epoch.
        assert_eq!(relative_time(0, NOW, Lang::En), "2,976 wk. ago");
    }

    fn trigger(left: f32, right: f32) -> Rect {
        Rect {
            left,
            top: 10.0,
            right,
            bottom: 32.0,
        }
    }

    #[test]
    fn 팝오버는_버튼_아래_6픽셀에_오른쪽_끝을_맞춘다() {
        let place = popover_placement(trigger(800.0, 880.0), None, 1000.0);
        assert_eq!(
            place,
            Placement {
                top: 38.0,
                left: 560.0,
                width: 320.0
            }
        );
    }

    #[test]
    fn 팝오버는_칼럼_안에_8픽셀_여백을_두고_머문다() {
        let column = Some(Span {
            left: 600.0,
            right: 1000.0,
        });
        // The button's right edge is too far left: the popover stops at the column's left.
        assert_eq!(
            popover_placement(trigger(610.0, 690.0), column, 1000.0).left,
            608.0
        );
        // And too far right: it stops 8px before the column's right edge.
        assert_eq!(
            popover_placement(trigger(950.0, 1000.0), column, 1000.0).left,
            672.0
        );
    }

    #[test]
    fn 좁은_칼럼에서는_팝오버가_칼럼_너비에서_16을_뺀_만큼이다() {
        let column = Some(Span {
            left: 700.0,
            right: 1000.0,
        });
        let place = popover_placement(trigger(900.0, 980.0), column, 1000.0);
        assert_eq!(place.width, 284.0);
        assert_eq!(place.left, 708.0);
    }

    #[test]
    fn 저장된_대화에서_메뉴의_줄을_만든다() {
        let conversation = ChatConversation {
            id: "c1".into(),
            agent_id: AcpAgentId::Codex,
            messages: vec![serde_json::json!({ "role": "user" })],
            title: "hello".into(),
            created_at: 1,
            updated_at: 2,
        };
        assert_eq!(
            HistoryRow::from(&conversation),
            HistoryRow {
                id: "c1".into(),
                title: "hello".into(),
                agent_id: AcpAgentId::Codex,
                updated_at: 2
            }
        );
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{click, look, window};

    fn rows() -> Vec<HistoryRow> {
        ["first", "second"]
            .iter()
            .enumerate()
            .map(|(index, title)| HistoryRow {
                id: format!("c{index}"),
                title: (*title).to_owned(),
                agent_id: AcpAgentId::ClaudeCode,
                updated_at: 0,
            })
            .collect()
    }

    fn menu(
        disabled: bool,
        cx: &mut TestAppContext,
    ) -> (
        Entity<ChatHistoryMenu>,
        Rc<RefCell<Vec<ChatHistoryEvent>>>,
        &mut VisualTestContext,
    ) {
        let events: Rc<RefCell<Vec<ChatHistoryEvent>>> = Rc::default();
        let seen = events.clone();
        let (menu, cx) = window(cx, move |_, cx| {
            cx.subscribe(&cx.entity(), move |_, _, event: &ChatHistoryEvent, _| {
                seen.borrow_mut().push(event.clone());
            })
            .detach();
            ChatHistoryMenu::new(
                move |_| ChatHistoryMenuProps {
                    conversations: rows(),
                    current_id: Some("c1".into()),
                    disabled,
                    column: None,
                },
                look(),
            )
        });
        (menu, events, cx)
    }

    fn is_open(menu: &Entity<ChatHistoryMenu>, cx: &mut VisualTestContext) -> bool {
        menu.read_with(cx, |menu, _| menu.is_open())
    }

    #[gpui::test]
    fn 버튼을_누르면_열리고_다시_누르면_닫힌다(cx: &mut TestAppContext) {
        let (menu, _, cx) = menu(false, cx);
        click("chat-history-trigger", cx);
        assert!(is_open(&menu, cx));
        let trigger = cx.debug_bounds("chat-history-trigger").unwrap();
        let popover = cx.debug_bounds("chat-history-popover").unwrap();
        assert_eq!(popover.top(), trigger.bottom() + px(6.0));
        assert_eq!(popover.size.width, px(320.0));
        click("chat-history-trigger", cx);
        assert!(!is_open(&menu, cx));
    }

    #[gpui::test]
    fn 비활성이면_열리지_않는다(cx: &mut TestAppContext) {
        let (menu, _, cx) = menu(true, cx);
        click("chat-history-trigger", cx);
        assert!(!is_open(&menu, cx));
    }

    #[gpui::test]
    fn 줄을_누르면_그_대화를_열라고_알리고_닫힌다(cx: &mut TestAppContext) {
        let (menu, events, cx) = menu(false, cx);
        click("chat-history-trigger", cx);
        click("chat-history-open-1", cx);
        assert_eq!(*events.borrow(), [ChatHistoryEvent::Open("c1".into())]);
        assert!(!is_open(&menu, cx));
    }

    #[gpui::test]
    fn x를_누르면_지우라고_알리고_열린_채로_있다(cx: &mut TestAppContext) {
        let (menu, events, cx) = menu(false, cx);
        click("chat-history-trigger", cx);
        click("chat-history-delete-0", cx);
        assert_eq!(*events.borrow(), [ChatHistoryEvent::Remove("c0".into())]);
        assert!(is_open(&menu, cx));
    }

    #[gpui::test]
    fn 바깥을_누르면_닫힌다(cx: &mut TestAppContext) {
        let (menu, events, cx) = menu(false, cx);
        click("chat-history-trigger", cx);
        cx.simulate_click(point(px(900.0), px(700.0)), Modifiers::none());
        cx.run_until_parked();
        assert!(!is_open(&menu, cx));
        assert!(events.borrow().is_empty());
    }
}
