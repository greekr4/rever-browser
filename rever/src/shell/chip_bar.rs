//! The chip bar at the bottom of the page area and the panel a chip opens above it.
//! Port of `src/renderer/src/components/FloatingChips.tsx`.

use gpui::{AnyView, ClickEvent, Context, FontWeight, div, prelude::*, px};
use rever_ui_kit::theme::Theme;

use super::icon;
use crate::app::{Browser, PANEL_GROUPS, panel_title_key};

const PANEL_HEIGHT: f32 = 320.0;

/// `n.toLocaleString('en-US')`: the count on a chip.
fn fmt_count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

impl Browser {
    /// The view of a ported panel; `None` for the ones that still show the notice.
    fn panel_view(&self, panel: &str) -> Option<AnyView> {
        let views = &self.views;
        Some(match panel {
            "traffic" => views.traffic.clone().into(),
            "console" => views.console.clone().into(),
            "exceptions" => views.exceptions.clone().into(),
            "websocket" => views.websocket.clone().into(),
            "history" => views.history.clone().into(),
            _ => return None,
        })
    }

    pub(super) fn panel(
        &self,
        panel: &'static str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let count = self.panel_count(panel, cx);
        let hover = theme.chrome_hover;
        let body = match self.panel_view(panel) {
            // The views scroll on their own.
            Some(view) => div().flex_1().min_h_0().child(view).into_any_element(),
            None => div()
                .flex_1()
                .p(px(24.0))
                .text_color(theme.text_dim)
                .child(self.lang.t("port.panelNotYet"))
                .into_any_element(),
        };
        // `.chip-panel-bottom`
        div()
            .h(px(PANEL_HEIGHT))
            .flex()
            .flex_col()
            .flex_none()
            .bg(theme.bg)
            .border_t_1()
            .border_color(theme.border_2)
            .child(
                // `.chip-panel-header`
                div()
                    .px(px(14.0))
                    .py(px(8.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .text_size(px(12.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.lang.t(panel_title_key(panel)))
                            .when(count > 0, |title| {
                                // `.chip-panel-count`
                                title.child(
                                    div()
                                        .font_weight(FontWeight::NORMAL)
                                        .text_color(theme.text_dim)
                                        .child(count.to_string()),
                                )
                            }),
                    )
                    .child(
                        // `.chip-panel-close`
                        div()
                            .id("panel-close")
                            .px(px(6.0))
                            .py(px(2.0))
                            .rounded(px(4.0))
                            .text_color(theme.text_dim)
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.open_panel = None;
                                cx.notify();
                            }))
                            .child("✕"),
                    ),
            )
            .child(body)
    }

    pub(super) fn chip_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = self.lang;
        let mut bar = div()
            .h(px(40.0))
            .px(px(8.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .overflow_hidden()
            .bg(theme.bg_bar)
            .border_t_1()
            .border_color(theme.border);
        for (group_index, group) in PANEL_GROUPS.iter().enumerate() {
            if group_index > 0 {
                bar = bar.child(
                    div()
                        .w(px(1.0))
                        .h(px(16.0))
                        .mx(px(6.0))
                        .flex_none()
                        .bg(theme.border_2),
                );
            }
            for &panel in *group {
                let open = self.open_panel == Some(panel);
                let color = if open { theme.text } else { theme.text_dim };
                let hover = theme.chrome_hover;
                let count = Some(self.panel_count(panel, cx)).filter(|count| *count > 0);
                // `.chip-count.danger`: uncaught exceptions stand out.
                let count_color = if panel == "exceptions" {
                    theme.status_error
                } else {
                    theme.text_2
                };
                bar = bar.child(
                    // `.chrome-chip`
                    div()
                        .id(panel)
                        .h(px(28.0))
                        .px(px(10.0))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(5.0))
                        .rounded(px(14.0))
                        .text_size(px(12.0))
                        .text_color(color)
                        .cursor_pointer()
                        .when(open, |chip| chip.bg(theme.surface_3))
                        .hover(move |style| style.bg(hover))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_panel(panel, cx)
                        }))
                        .child(icon(panel, 14.0, color))
                        .child(lang.t(panel_title_key(panel)))
                        .when_some(count, |chip, count| {
                            chip.child(
                                div()
                                    .px(px(6.0))
                                    .rounded(px(8.0))
                                    .bg(theme.surface_3)
                                    .text_size(px(11.0))
                                    .text_color(count_color)
                                    .child(fmt_count(count)),
                            )
                        }),
                );
            }
        }
        bar.child(div().flex_1()).child(
            div()
                .h(px(28.0))
                .px(px(10.0))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(theme.border_2)
                .text_size(px(12.0))
                .text_color(theme.status_error)
                .child(icon("redteam", 14.0, theme.status_error))
                .child(lang.t("panel.redteam")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 칩의_숫자는_세_자리마다_쉼표를_넣는다() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1000), "1,000");
        assert_eq!(fmt_count(1234567), "1,234,567");
    }
}
