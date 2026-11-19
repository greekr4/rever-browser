//! The chat panel on the right: the conversation and the composer.

use gpui::{Context, div, prelude::*, px};
use rever_ui_kit::theme::Theme;
use rever_ui_markdown::markdown;

use super::{icon, icon_button};
use crate::app::{Browser, link_target};

pub(super) const CHAT_WIDTH: f32 = 420.0;
const CHAT_EMPTY: &str = "Open a site in the browser and ask me to analyze its API traffic, or describe an action you just performed and I'll dig into the requests it triggered.";

impl Browser {
    pub(super) fn chat_panel(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = self.lang;
        let has_text = !self.views.composer.read(cx).text().is_empty();
        let this = cx.weak_entity();
        let footer_chip = |name: &'static str, label: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_size(px(12.0))
                .text_color(theme.text_dim)
                .child(icon(name, 13.0, theme.text_dim))
                .child(label)
        };
        div()
            .w(px(CHAT_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .bg(theme.bg)
            .border_l_1()
            .border_color(theme.border_2)
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
                            .child("Claude Code")
                            .child(icon("close", 10.0, theme.text_dim)),
                    )
                    .child(div().pb(px(2.0)).child(icon_button(
                        "chat-new-tab",
                        "plus",
                        28.0,
                        theme,
                    ))),
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
                    .child(format!("{} (0)", lang.t("chat.history")))
                    .child(
                        div()
                            .px(px(8.0))
                            .py(px(3.0))
                            .rounded(px(6.0))
                            .bg(theme.surface_2)
                            .child(lang.t("chat.newShort")),
                    ),
            )
            .child(
                div()
                    .id("chat-log")
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
                    .children(self.chat.iter().enumerate().map(|(index, line)| {
                        if line.from_user {
                            // `.chat-user-bubble`
                            div()
                                .flex()
                                .justify_end()
                                .child(
                                    div()
                                        .max_w(px(CHAT_WIDTH - 80.0))
                                        .px(px(12.0))
                                        .py(px(8.0))
                                        .rounded(px(14.0))
                                        .bg(theme.surface_2)
                                        .child(line.text.clone()),
                                )
                                .into_any_element()
                        } else if line.failed {
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.status_error)
                                .child(line.text.clone())
                                .into_any_element()
                        } else {
                            // Parsed again on every render, so a streaming reply grows in place.
                            let this = this.clone();
                            markdown(&line.text, theme)
                                .id(("chat-message", index))
                                .lang(lang)
                                .on_link(move |href, window, cx| {
                                    let Some(url) = link_target(href) else {
                                        return;
                                    };
                                    let _ =
                                        this.update(cx, |this, cx| this.open_tab(url, window, cx));
                                })
                                .into_any_element()
                        }
                    })),
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
                                .child(footer_chip("shield", lang.t("chat.perm.auto")))
                                .child(footer_chip("wrench", "Recon"))
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(theme.text_dim)
                                        .child("Claude Code"),
                                )
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
