//! The Red Team scan dialog — a movable popup ported from the renderer's
//! `ScanPopup.tsx`: pick a target, page budget and intensity, then Start hands
//! the run to the scan orchestrator. While it runs the config rows collapse to
//! a stats line (pages / findings / elapsed); the finished report opens in its
//! own window (`shell::report`), with Copy / Save / Send-to-chat here.

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

/// `MM:SS`, the log's timestamp and the stats row's clock.
fn mmss(secs: u32) -> String {
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

impl Browser {
    /// The risk colour + label key for a scan intensity.
    fn risk(&self, intensity: &str, theme: &Theme) -> (Hsla, &'static str) {
        match intensity {
            "readonly" => (rgb(0x5f_d0_7a).into(), "scan.risk.safe"),
            "probe" => (rgb(0xe0_a3_3e).into(), "scan.risk.caution"),
            _ => (theme.status_error.into(), "scan.risk.danger"),
        }
    }

    pub(super) fn redteam_dialog(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.redteam_open {
            return None;
        }
        let lang = self.lang;
        let (rx, ry) = self.redteam_pos;
        let hover = theme.chrome_hover;
        let running = self.redteam_running;
        let scan = self.scan.as_ref();

        // One intensity button: a risk dot + label, highlighted when selected.
        let intensity_btn =
            |level: &'static str, label_key: &'static str, color: u32, cx: &mut Context<Self>| {
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
                    .border_color(if selected {
                        theme.accent
                    } else {
                        theme.border_2
                    })
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

        // A small bordered action button (Copy, Save, Send to chat).
        let action_btn = |id: &'static str, label: &str| {
            div()
                .id(id)
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(5.0))
                .border_1()
                .border_color(theme.border_2)
                .text_size(px(12.0))
                .text_color(theme.text)
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(label.to_owned())
        };

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
                    .text_color(theme.text)
                    .child(lang.t("scan.notify.title")),
            )
            .when(running, |header| {
                header.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.accent)
                        .child(lang.t("scan.panel.running")),
                )
            })
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

        // ── Config (idle or after a finished run) / stats (while running) ──────
        let top: AnyElement = if running {
            let (pages_seen, findings, elapsed) = scan.map_or((0, 0, 0), |scan| {
                (scan.pages_seen(), scan.findings, scan.elapsed_secs())
            });
            let stat = |dim: &str, value: String| {
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(4.0))
                    .child(div().text_color(theme.text_dim).child(dim.to_owned()))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(value),
                    )
            };
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(14.0))
                .child(stat(
                    lang.t("scan.panel.pages"),
                    format!("{pages_seen}/{}", self.redteam_pages),
                ))
                .child(stat(lang.t("scan.panel.findings"), findings.to_string()))
                .child(div().text_color(theme.text_dim).child(mmss(elapsed)))
                .child(div().flex_1())
                .child(
                    div()
                        .id("redteam-stop")
                        .px(px(12.0))
                        .py(px(5.0))
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(theme.status_error)
                        .text_color(theme.status_error)
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop_redteam(cx)))
                        .child(format!("■ {}", lang.t("scan.panel.stop"))),
                )
                .into_any_element()
        } else {
            let target_row = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_color(theme.text_dim)
                        .child(lang.t("scan.panel.target")),
                )
                .child(div().flex_1().child(self.views.redteam_target.clone()))
                .child(
                    div()
                        .text_color(theme.text_dim)
                        .child(lang.t("scan.bar.pages")),
                )
                .child(div().w(px(54.0)).child(self.views.redteam_pages.clone()))
                .child(
                    div()
                        .id("redteam-start")
                        .px(px(14.0))
                        .py(px(6.0))
                        .rounded(px(6.0))
                        .bg(theme.accent)
                        .text_color(theme.bg)
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .hover(move |s| s.opacity(0.9))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_redteam(cx)))
                        .child(format!("▶ {}", lang.t("scan.panel.start"))),
                );

            let intensity_row = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_color(theme.text_dim)
                        .child(lang.t("scan.panel.intensity")),
                )
                .child(intensity_btn(
                    "readonly",
                    "scan.panel.intensity.readonly",
                    0x5f_d0_7a,
                    cx,
                ))
                .child(intensity_btn(
                    "probe",
                    "scan.panel.intensity.probe",
                    0xe0_a3_3e,
                    cx,
                ))
                .child(intensity_btn(
                    "redteam",
                    "scan.panel.intensity.redteam",
                    0,
                    cx,
                ))
                // The scan's model chip; the sessions spawn on this model.
                .child(self.scan_model_picker.clone());

            let risk_row = div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .text_color(theme.text_dim)
                        .child(lang.t("scan.panel.risk")),
                )
                .child(div().w(px(8.0)).h(px(8.0)).rounded(px(8.0)).bg(risk_color))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(risk_color)
                        .child(lang.t(risk_key)),
                );

            div()
                .flex()
                .flex_none()
                .flex_col()
                .gap(px(10.0))
                .child(target_row)
                .child(intensity_row)
                .child(risk_row)
                .into_any_element()
        };

        // ── The finished report: open it in its own window, or hand it over ───
        let actions: Option<AnyElement> = if !running {
            scan.map(|scan| {
                if scan.report.is_empty() {
                    return div()
                        .text_color(theme.text_dim)
                        .child(lang.t("scan.report.empty"))
                        .into_any_element();
                }
                let report = scan.report.clone();
                let copy_text = report.clone();
                let draft_text = report.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .pt(px(2.0))
                    .child(
                        div()
                            .id("redteam-report")
                            .px(px(12.0))
                            .py(px(5.0))
                            .rounded(px(6.0))
                            .bg(theme.accent)
                            .text_color(theme.bg)
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .hover(move |s| s.opacity(0.9))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.open_report_window(cx)
                            }))
                            .child(lang.t("scan.panel.report")),
                    )
                    .child(
                        action_btn("redteam-copy", lang.t("scan.panel.copy")).on_click(
                            cx.listener(move |_, _: &ClickEvent, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    copy_text.clone(),
                                ));
                            }),
                        ),
                    )
                    .child(
                        action_btn("redteam-save", lang.t("scan.panel.save")).on_click(
                            cx.listener(move |this, _: &ClickEvent, _, _| {
                                if let Some(scan) = this.scan.as_ref() {
                                    if let Ok(path) = crate::shell::report::save_report_markdown(
                                        scan.target(),
                                        &scan.report,
                                    ) {
                                        #[cfg(target_os = "macos")]
                                        std::process::Command::new("open")
                                            .arg("-R")
                                            .arg(&path)
                                            .spawn()
                                            .ok();
                                    }
                                }
                            }),
                        ),
                    )
                    .child(
                        action_btn("redteam-send", lang.t("scan.panel.sendChat")).on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.push_draft(&draft_text, cx);
                                this.close_redteam(cx);
                            }),
                        ),
                    )
                    .into_any_element()
            })
        } else {
            None
        };

        // ── Terminal-style activity log, collapsible like the TS popup ─────────
        // Fixed light-on-dark palette: the box stays dark in both themes.
        let mut lines: Vec<AnyElement> = Vec::new();
        if let Some(scan) = scan {
            for line in &scan.log {
                let (glyph, color) = match line.kind {
                    crate::scan::LineKind::Phase => ("»", rgb(0x7a_b8_ff)),
                    crate::scan::LineKind::Tool => ("›", rgb(0xb6_bd_c8)),
                    crate::scan::LineKind::Finding => ("✓", rgb(0x5f_d0_7a)),
                    crate::scan::LineKind::Error => ("✕", rgb(0xff_6b_6b)),
                };
                let tag = line
                    .tab
                    .as_ref()
                    .map(|tab| format!("[{tab}]"))
                    .unwrap_or_default();
                lines.push(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex_none()
                                .text_color(rgb(0x8a_8f_98))
                                .child(mmss(line.at)),
                        )
                        .child(div().flex_none().text_color(color).child(glyph))
                        .when(!tag.is_empty(), |row| {
                            row.child(
                                div()
                                    .flex_none()
                                    .max_w(px(110.0))
                                    .truncate()
                                    .text_color(rgb(0x6a_8f_6a))
                                    .child(tag),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .text_color(match line.kind {
                                    crate::scan::LineKind::Tool => rgb(0xb6_bd_c8),
                                    _ => color,
                                })
                                .line_height(relative(1.4))
                                .child(line.text.clone()),
                        )
                        .into_any_element(),
                );
            }
        }
        let log_open = self.redteam_log_open;
        let log_count = scan.map_or(0, |scan| scan.log.len());
        let log_section = scan
            .filter(|scan| !scan.log.is_empty())
            .map(|_| {
                div()
                    .flex()
                    .flex_col()
                    .when_some(log_open.then_some(()), |section, _| {
                        section.child(
                            div()
                                .id("redteam-log")
                                .h(px(170.0))
                                .flex_none()
                                .overflow_y_scroll()
                                .track_scroll(&self.redteam_log_scroll)
                                .p(px(8.0))
                                .rounded(px(6.0))
                                .bg(rgb(0x0b_0d_10))
                                .text_size(px(11.0))
                                .font_family("monospace")
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .children(lines),
                        )
                    })
            })
            .map(|body| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .id("redteam-log-toggle")
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_dim)
                            .cursor_pointer()
                            .hover(move |s| s.opacity(0.8))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.redteam_log_open = !this.redteam_log_open;
                                cx.notify();
                            }))
                            .child(if log_open { "▾" } else { "▸" })
                            .child(lang.t("scan.panel.log"))
                            .child(
                                div()
                                    .text_color(theme.text_dim)
                                    .child(format!("({log_count})")),
                            ),
                    )
                    .when(log_open, |section| section.child(body))
            });

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
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<RedteamDrag>, _, cx| {
                    this.on_redteam_drag(
                        f32::from(event.event.position.x),
                        f32::from(event.event.position.y),
                        cx,
                    )
                }),
            )
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<RedteamResize>, _, cx| {
                    this.on_redteam_resize(
                        f32::from(event.event.position.x),
                        f32::from(event.event.position.y),
                        cx,
                    )
                }),
            )
            .child(header)
            .child(
                div()
                    .id("redteam-body")
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .flex_col()
                    .gap(px(10.0))
                    .overflow_y_scroll()
                    .p(px(12.0))
                    .child(top)
                    .children(actions)
                    .children(log_section),
            )
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
