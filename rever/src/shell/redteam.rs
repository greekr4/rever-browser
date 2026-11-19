//! The Red Team scan dialog — a movable popup ported from the renderer's
//! `ScanPopup.tsx` (top half): pick a target, intensity and page budget, then
//! Start hands the run to the agent (`redteam_start`). The live log / report
//! streaming of the original needs the scan-store subsystem, which isn't ported.

use gpui::{
    AnyElement, ClickEvent, Context, DragMoveEvent, FontWeight, Hsla, MouseButton, MouseDownEvent,
    div, prelude::*, px, relative, rgb,
};
use rever_ui_kit::theme::Theme;

use crate::app::Browser;

/// Marker for the dialog's header move-drag.
struct RedteamDrag;

/// Marker for the dialog's bottom-right resize-drag.
struct RedteamResize;

impl Browser {
    /// The risk colour + label key for a scan intensity.
    fn risk(&self, intensity: &str, theme: &Theme) -> (Hsla, &'static str) {
        match intensity {
            "readonly" => (rgb(0x5f_d0_7a).into(), "scan.risk.safe"),
            "probe" => (rgb(0xe0_a3_3e).into(), "scan.risk.caution"),
            _ => (theme.status_error.into(), "scan.risk.danger"),
        }
    }

    pub(super) fn redteam_dialog(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.redteam_open {
            return None;
        }
        let lang = self.lang;
        let (rx, ry) = self.redteam_pos;
        let hover = theme.chrome_hover;

        // One intensity button: a risk dot + label, highlighted when selected.
        let intensity_btn = |level: &'static str, label_key: &'static str, color: u32, cx: &mut Context<Self>| {
            let selected = self.redteam_intensity == level;
            let dot: Hsla = if level == "redteam" {
                theme.status_error.into()
            } else {
                rgb(color).into()
            };
            div()
                .id(level)
                .px(px(8.0))
                .py(px(3.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                .rounded(px(5.0))
                .border_1()
                .border_color(if selected { theme.accent } else { theme.border_2 })
                .text_color(if selected { theme.text } else { theme.text_dim })
                .when(selected, |b| b.bg(theme.surface_3))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_redteam_intensity(level, cx)
                }))
                .child(div().w(px(7.0)).h(px(7.0)).rounded(px(7.0)).bg(dot))
                .child(lang.t(label_key))
        };

        let (risk_color, risk_key) = self.risk(self.redteam_intensity, theme);

        let header = div()
            .id("redteam-header")
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(8.0))
            .bg(theme.bg_bar)
            .border_b_1()
            .border_color(theme.border)
            .cursor(gpui::CursorStyle::OpenHand)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, _| {
                    this.redteam_drag = Some((
                        f32::from(event.position.x) - this.redteam_pos.0,
                        f32::from(event.position.y) - this.redteam_pos.1,
                    ));
                }),
            )
            .on_drag(RedteamDrag, |_, _, _, cx| cx.new(|_| gpui::Empty))
            .child(
                div()
                    .flex_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.status_error)
                    .child(lang.t("panel.redteam")),
            )
            .child(
                div()
                    .id("redteam-close")
                    .px(px(6.0))
                    .rounded(px(4.0))
                    .text_color(theme.text_dim)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_redteam(cx)))
                    .child("✕"),
            );

        let target_row = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(div().text_color(theme.text_dim).child(lang.t("scan.panel.target")))
            .child(div().flex_1().child(self.views.redteam_target.clone()));

        let intensity_row = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(div().text_color(theme.text_dim).child(lang.t("scan.panel.intensity")))
            .child(intensity_btn("readonly", "scan.panel.intensity.readonly", 0x5f_d0_7a, cx))
            .child(intensity_btn("probe", "scan.panel.intensity.probe", 0xe0_a3_3e, cx))
            .child(intensity_btn("redteam", "scan.panel.intensity.redteam", 0, cx));

        let risk_row = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(div().text_color(theme.text_dim).child(lang.t("scan.panel.risk")))
            .child(div().w(px(8.0)).h(px(8.0)).rounded(px(8.0)).bg(risk_color))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(risk_color)
                    .child(lang.t(risk_key)),
            )
            .child(div().flex_1())
            .child(div().text_color(theme.text_dim).child(format!("{} {}", self.redteam_pages, lang.t("scan.panel.pages"))));

        let running = self.redteam_running;
        let start = if running {
            div()
                .id("redteam-stop")
                .px(px(14.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .bg(theme.status_error)
                .text_color(theme.bg)
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop_redteam(cx)))
                .child(format!("■ {}", lang.t("scan.panel.stop")))
                .into_any_element()
        } else {
            div()
                .id("redteam-start")
                .px(px(14.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .bg(theme.accent)
                .text_color(theme.bg)
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_redteam(cx)))
                .child(format!("▶ {}", lang.t("scan.panel.start")))
                .into_any_element()
        };

        // The scan's structured activity log: one line per event, the worker it
        // came from as a short [tag], findings in the warning colour, phase
        // lines set apart; the finished report renders as Markdown.
        let mut lines: Vec<AnyElement> = Vec::new();
        if let Some(scan) = &self.scan {
            for line in &scan.log {
                let (color, weight, spaced) = match line.kind {
                    crate::scan::LineKind::Phase => (theme.text, FontWeight::SEMIBOLD, true),
                    crate::scan::LineKind::Tool => (theme.text_dim, FontWeight::NORMAL, false),
                    crate::scan::LineKind::Finding => (rgb(0xe0_a3_3e).into(), FontWeight::NORMAL, true),
                    crate::scan::LineKind::Error => (theme.status_error.into(), FontWeight::NORMAL, true),
                };
                let tag = line
                    .tab
                    .as_ref()
                    .map(|tab| format!("[{tab}]"))
                    .unwrap_or_default();
                lines.push(
                    div()
                        .when(spaced, |line| line.mt(px(6.0)))
                        .flex()
                        .items_start()
                        .gap(px(6.0))
                        .when_some(
                            (!tag.is_empty()).then_some(tag),
                            |row, tag| {
                                row.child(
                                    div()
                                        .flex_none()
                                        .max_w(px(110.0))
                                        .truncate()
                                        .text_color(theme.text_dim)
                                        .child(tag),
                                )
                            },
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_color(color)
                                .font_weight(weight)
                                .line_height(relative(1.35))
                                .child(line.text.clone()),
                        )
                        .into_any_element(),
                );
            }
            if !running && !scan.report.is_empty() {
                lines.push(
                    div()
                        .mt(px(10.0))
                        .pt(px(10.0))
                        .border_t_1()
                        .border_color(theme.border)
                        .text_color(theme.text)
                        .child(
                            rever_ui_markdown::markdown::markdown(&scan.report, theme)
                                .id("redteam-report")
                                .lang(lang),
                        )
                        .into_any_element(),
                );
            }
        } else if running {
            lines.push(
                div()
                    .text_color(theme.text_dim)
                    .child(lang.t("scan.panel.running"))
                    .into_any_element(),
            );
        }
        let log = div()
            .id("redteam-log")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(10.0))
            .bg(theme.bg_bar)
            .border_t_1()
            .border_color(theme.border)
            .text_size(px(11.0))
            .text_color(theme.text_dim)
            .font_family("monospace")
            .children(lines);

        let dialog = div()
            .absolute()
            .left(px(rx))
            .top(px(ry))
            .w(px(self.redteam_w))
            .h(px(self.redteam_h))
            .occlude()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.0))
            .shadow_lg()
            .text_size(px(12.0))
            .text_color(theme.text)
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<RedteamDrag>, _, cx| {
                this.on_redteam_drag(
                    f32::from(event.event.position.x),
                    f32::from(event.event.position.y),
                    cx,
                )
            }))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<RedteamResize>, _, cx| {
                this.on_redteam_resize(
                    f32::from(event.event.position.x),
                    f32::from(event.event.position.y),
                    cx,
                )
            }))
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_col()
                    .gap(px(12.0))
                    .p(px(14.0))
                    .child(target_row)
                    .child(intensity_row)
                    .child(risk_row)
                    .child(div().flex().justify_end().child(start)),
            )
            .child(log)
            // Bottom-right corner: drag to resize.
            .child(
                div()
                    .id("redteam-resize")
                    .absolute()
                    .bottom_0()
                    .right_0()
                    .w(px(14.0))
                    .h(px(14.0))
                    .cursor(gpui::CursorStyle::ResizeUpLeftDownRight)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, _| {
                            this.redteam_size_drag = Some((
                                f32::from(event.position.x),
                                f32::from(event.position.y),
                                this.redteam_w,
                                this.redteam_h,
                            ));
                        }),
                    )
                    .on_drag(RedteamResize, |_, _, _, cx| cx.new(|_| gpui::Empty)),
            );
        Some(dialog.into_any_element())
    }
}
