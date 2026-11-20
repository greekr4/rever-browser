//! The chat panel on the right: the conversation and the composer.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, ClickEvent, Context, DragMoveEvent, MouseButton,
    MouseDownEvent, Transformation, div, percentage, prelude::*, px, svg,
};
use rever_ui_kit::theme::Theme;
use rever_ui_markdown::markdown;

use super::{icon, icon_button};
use crate::app::{Browser, ChatLine, link_target, shows_end};

/// The chat panel's width until the user drags its edge.
pub(crate) const CHAT_WIDTH: f32 = 420.0;
/// Narrower than this the composer's controls no longer fit in a row.
pub(crate) const CHAT_MIN_WIDTH: f32 = 320.0;
/// What the page keeps beside the chat panel, when the window has it to give.
const PAGE_MIN_WIDTH: f32 = 360.0;

/// Dragged from the panel's left edge to resize it.
struct ChatDrag;


/// How wide the chat panel is drawn in a window `window` wide: nothing when folded,
/// the whole window when expanded, else the user's width, cut back so the page keeps
/// some room and never past the window itself.
pub(super) fn chat_panel_width(window: f32, wanted: f32, collapsed: bool, expanded: bool) -> f32 {
    if collapsed {
        return 0.0;
    }
    if expanded {
        return window;
    }
    let most = (window - PAGE_MIN_WIDTH).max(CHAT_MIN_WIDTH);
    wanted.clamp(CHAT_MIN_WIDTH, most).min(window)
}
const CHAT_EMPTY: &str = "Open a site in the browser and ask me to analyze its API traffic, or describe an action you just performed and I'll dig into the requests it triggered.";

/// Opacity of a pulsing dot at `progress` (0..1) of its cycle.
fn pulse(progress: f32) -> f32 {
    let wave = (progress * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    0.25 + 0.75 * wave
}

/// How long the turn has run: `42s`, then `2m 5s`.
fn elapsed_label(seconds: u64) -> String {
    match seconds / 60 {
        0 => format!("{seconds}s"),
        minutes => format!("{minutes}m {}s", seconds % 60),
    }
}

impl Browser {
    pub(super) fn chat_panel(
        &self,
        theme: &Theme,
        width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let lang = self.lang;
        let has_text = !self.views.composer.read(cx).text().is_empty();
        let at_end = shows_end(
            self.chat_scroll.offset().y.into(),
            self.chat_scroll.max_offset().height.into(),
        );
        let this = cx.weak_entity();
        let footer_chip = |name: &'static str, label: &'static str| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.0))
                .text_size(px(12.0))
                .text_color(theme.text_dim)
                .child(icon(name, 13.0, theme.text_dim))
                .child(label)
        };
        // The conversation: the user's bubbles, the agent's replies as Markdown, and each
        // run of tool calls folded into one line, as Orca's chat shows them.
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut index = 0;
        while index < self.chat.len() {
            let line = &self.chat[index];
            if line.tool.is_some() {
                let calls: Vec<&ChatLine> = self.chat[index..]
                    .iter()
                    .take_while(|line| line.tool.is_some())
                    .collect();
                let running = calls.iter().rev().find(|call| call.tool == Some(false));
                let key = if running.is_some() {
                    "chat.tools.running"
                } else {
                    "chat.tools.ran"
                };
                rows.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .text_size(px(13.0))
                        .text_color(theme.text_dim)
                        .child(icon("wrench", 13.0, theme.text_dim))
                        .child(lang.t(key).replace("{n}", &calls.len().to_string()))
                        .map(|row| match running {
                            // The call in flight, and a dot that pulses until it returns.
                            Some(call) => row
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family("Menlo")
                                        .text_size(px(12.0))
                                        .child(call.text.clone()),
                                )
                                .child(
                                    div()
                                        .size(px(6.0))
                                        .flex_none()
                                        .rounded_full()
                                        .bg(theme.accent)
                                        .with_animation(
                                            ("chat-tool-running", index),
                                            Animation::new(Duration::from_secs(1)).repeat(),
                                            |dot, progress| dot.opacity(pulse(progress)),
                                        ),
                                ),
                            None => row.child("✓"),
                        })
                        .into_any_element(),
                );
                index += calls.len();
                continue;
            }
            if line.from_user {
                // `.chat-user-bubble`
                rows.push(
                    div()
                        .flex()
                        .justify_end()
                        .child(
                            div()
                                .max_w(px(width - 80.0))
                                .px(px(12.0))
                                .py(px(8.0))
                                .rounded(px(12.0))
                                .border_1()
                                .border_color(theme.border_2)
                                .bg(theme.surface_2)
                                .child(line.text.clone()),
                        )
                        .into_any_element(),
                );
            } else if line.failed {
                rows.push(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.status_error)
                        .child(line.text.clone())
                        .into_any_element(),
                );
            } else if !line.text.is_empty() {
                // Parsed again on every render, so a streaming reply grows in place.
                let this = this.clone();
                rows.push(
                    markdown(&line.text, theme)
                        .id(("chat-message", index))
                        .lang(lang)
                        .on_link(move |href, window, cx| {
                            let Some(url) = link_target(href) else {
                                return;
                            };
                            let _ = this.update(cx, |this, cx| this.open_tab(url, window, cx));
                        })
                        .into_any_element(),
                );
            }
            index += 1;
        }
        div()
            .relative()
            .w(px(width))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .bg(theme.bg)
            .border_l_1()
            .border_color(theme.border_2)
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ChatDrag>, _, cx| {
                this.on_chat_drag(f32::from(event.event.position.x), cx)
            }))
            // A handle over the left edge resizes the panel; an expanded panel has no
            // edge to drag.
            .when(!self.chat_expanded, |panel| {
                panel.child(
                    div()
                        .id("chat-resize")
                        .absolute()
                        .left(px(-3.0))
                        .top_0()
                        .bottom_0()
                        .w(px(6.0))
                        .cursor_col_resize()
                        .hover(|style| style.bg(gpui::rgba(0x4a8fff66)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, _| {
                                this.chat_drag = Some((f32::from(event.position.x), width));
                            }),
                        )
                        .on_drag(ChatDrag, |_, _, _, cx| cx.new(|_| gpui::Empty)),
                )
            })
            .child(
                // `.chat-tabs`
                div()
                    .h(px(40.0))
                    .px(px(8.0))
                    .flex()
                    .flex_none()
                    .items_end()
                    .gap(px(2.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .h(px(32.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .rounded_t(px(8.0))
                            .bg(theme.surface_3)
                            .text_size(px(12.0))
                            .child(self.chat_agent_name())
                            .child(icon("close", 10.0, theme.text_dim)),
                    )
                    .child(div().pb(px(2.0)).child(icon_button(
                        "chat-new-tab",
                        "plus",
                        28.0,
                        theme,
                    )))
                    .child(div().flex_1())
                    // Let the panel cover the page, or fold it away.
                    .child(
                        div().pb(px(2.0)).child(
                            icon_button(
                                "chat-expand",
                                if self.chat_expanded { "shrink" } else { "expand" },
                                28.0,
                                theme,
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.toggle_chat_expanded(cx)
                            })),
                        ),
                    )
                    .child(
                        div().pb(px(2.0)).child(
                            icon_button("chat-collapse", "panel-right", 28.0, theme).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_chat(cx)),
                            ),
                        ),
                    ),
            )
            .child(
                div()
                    .h(px(36.0))
                    .px(px(12.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_end()
                    .gap(px(14.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(if self.agent_busy {
                        "streaming"
                    } else {
                        "ready"
                    })
                    .child(self.history_menu.clone())
                    .child(
                        div()
                            .id("chat-new")
                            .px(px(8.0))
                            .py(px(3.0))
                            .rounded(px(6.0))
                            .bg(theme.surface_2)
                            // A conversation is not dropped under a running turn.
                            .when(self.agent_busy, |button| button.opacity(0.42))
                            .when(!self.agent_busy, |button| {
                                button.cursor_pointer().on_click(cx.listener(
                                    |this, _: &ClickEvent, _, cx| this.new_conversation(cx),
                                ))
                            })
                            .child(lang.t("chat.newShort")),
                    ),
            )
            .child(
                // The log, and over it the way back down for a reader who scrolled up.
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .flex()
                    .flex_col()
                    .when(!at_end, |area| {
                        area.child(
                            div()
                                .id("chat-to-end")
                                .absolute()
                                .bottom(px(10.0))
                                .right(px(16.0))
                                .size(px(30.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .border_1()
                                .border_color(theme.border_2)
                                .bg(theme.chrome_toolbar)
                                .shadow_md()
                                .cursor_pointer()
                                .occlude()
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.chat_scroll.scroll_to_bottom();
                                    cx.notify();
                                }))
                                .child(icon("arrow-down", 15.0, theme.text_2)),
                        )
                    })
                    .child(
                div()
                    .id("chat-log")
                    .track_scroll(&self.chat_scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .text_size(px(14.0))
                    .when(self.chat.is_empty(), |log| {
                        log.child(div().text_color(theme.text_dim).child(CHAT_EMPTY))
                    })
                    .children(rows)
                    // A spinner while the agent works, so a silent stretch reads as work.
                    .when(self.agent_busy, |log| {
                        let seconds = self.turn_started.map_or(0, |at| at.elapsed().as_secs());
                        log.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .text_size(px(13.0))
                                .text_color(theme.text_dim)
                                .child(
                                    svg()
                                        .path("icons/loader.svg")
                                        .size(px(14.0))
                                        .flex_none()
                                        .text_color(theme.text_dim)
                                        .with_animation(
                                            "chat-working",
                                            Animation::new(Duration::from_secs(1)).repeat(),
                                            |spinner, progress| {
                                                spinner.with_transformation(Transformation::rotate(
                                                    percentage(progress),
                                                ))
                                            },
                                        ),
                                )
                                .child(format!(
                                    "{} {}",
                                    lang.t("chat.working"),
                                    elapsed_label(seconds)
                                )),
                        )
                    }),
                    ),
            )
            .child(
                div().p(px(12.0)).flex_none().child(
                    div()
                        .p(px(12.0))
                        .flex()
                        .flex_col()
                        .gap(px(12.0))
                        .rounded(px(16.0))
                        .border_1()
                        .border_color(theme.border_2)
                        .bg(theme.chrome_toolbar)
                        .child(div().text_size(px(14.0)).child(self.views.composer.clone()))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(14.0))
                                // In a narrow panel these give way, not the send button.
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .flex()
                                        .items_center()
                                        .gap(px(14.0))
                                        .child(footer_chip("shield", lang.t("chat.perm.auto")))
                                        .child(footer_chip("wrench", "Recon")),
                                )
                                .child(self.model_picker.clone())
                                .child(
                                    // `.composer-send`
                                    div()
                                        .size(px(32.0))
                                        .flex()
                                        .flex_none()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(if has_text {
                                            theme.accent
                                        } else {
                                            theme.border_2
                                        })
                                        .child(icon("send", 16.0, theme.chrome_toolbar)),
                                ),
                        ),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 채팅_패널은_창이_허락하는_만큼만_넓어진다() {
        // 넓은 창: 사용자가 정한 너비 그대로.
        assert_eq!(chat_panel_width(1440.0, 420.0, false, false), 420.0);
        assert_eq!(chat_panel_width(1440.0, 700.0, false, false), 700.0);
        // 페이지에 360은 남긴다.
        assert_eq!(chat_panel_width(900.0, 700.0, false, false), 540.0);
        // 좁은 창: 최소 너비를 지키되 창을 넘지는 않는다.
        assert_eq!(chat_panel_width(500.0, 420.0, false, false), 320.0);
        assert_eq!(chat_panel_width(280.0, 420.0, false, false), 280.0);
    }

    #[test]
    fn 접으면_너비가_없고_펼치면_창_전체다() {
        assert_eq!(chat_panel_width(1440.0, 420.0, true, false), 0.0);
        assert_eq!(chat_panel_width(1440.0, 420.0, false, true), 1440.0);
    }

    #[test]
    fn 작업_시간은_초로_쓰다가_1분부터_분과_초로_쓴다() {
        assert_eq!(elapsed_label(0), "0s");
        assert_eq!(elapsed_label(59), "59s");
        assert_eq!(elapsed_label(60), "1m 0s");
        assert_eq!(elapsed_label(778), "12m 58s");
    }
}
