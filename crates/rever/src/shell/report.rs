//! The finished scan's report in its own window — the Electron app kept the
//! report inside the popup; here it gets a dedicated window so a long report
//! stays readable while the dialog is closed. Content is a snapshot: reports
//! are final when they land.

use gpui::{ClipboardItem, Context, Render, Window, div, prelude::*, px, rgb};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};

/// Write the report to `~/Downloads/redteam-<host>-<date>.md` and return the
/// path (the Electron app "saved" via a browser download; this is the desktop
/// equivalent). Shared by the report window and the Red Team dialog.
pub(crate) fn save_report_markdown(
    target: &str,
    report: &str,
) -> std::io::Result<std::path::PathBuf> {
    let host = url::Url::parse(target)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "site".to_owned());
    let date = rever_core::time::to_iso_string(crate::app::now_ms())
        .and_then(|iso| iso.split('T').next().map(str::to_owned))
        .unwrap_or_else(|| "report".to_owned());
    let path = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join("Downloads")
        .join(format!("redteam-{host}-{date}.md"));
    std::fs::write(&path, report)?;
    Ok(path)
}

pub struct ReportWindow {
    target: String,
    report: String,
    findings: u32,
    lang: Lang,
    theme_mode: ThemeMode,
}

impl ReportWindow {
    pub fn new(target: &str, findings: u32, report: &str, lang: Lang, theme_mode: ThemeMode) -> Self {
        Self {
            target: target.to_owned(),
            report: report.to_owned(),
            findings,
            lang,
            theme_mode,
        }
    }

    /// Refresh what an already-open window shows (a new scan finished).
    pub fn set(&mut self, target: &str, findings: u32, report: &str) {
        self.target = target.to_owned();
        self.findings = findings;
        self.report = report.to_owned();
    }
}

impl Render for ReportWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.theme_mode, window.appearance());
        let lang = self.lang;
        let hover = theme.chrome_hover;
        let host = url::Url::parse(&self.target)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| self.target.clone());
        let findings = self.findings;
        let report = self.report.clone();

        let btn = |id: &'static str, label: &str| {
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
                .hover(move |style| style.bg(hover))
                .child(label.to_owned())
        };

        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.0))
            .px(px(14.0))
            .py(px(8.0))
            .bg(theme.bg_bar)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family("monospace")
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .child(host),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(0xe0_a3_3e))
                    .child(format!("{findings} {}", lang.t("scan.panel.findings"))),
            )
            .child(
                btn("report-copy", lang.t("scan.panel.copy")).on_click(
                    cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(this.report.clone()));
                    }),
                ),
            )
            .child(
                btn("report-save", lang.t("scan.panel.save")).on_click(
                    cx.listener(|this, _: &gpui::ClickEvent, _, _| {
                        if let Ok(path) = save_report_markdown(&this.target, &this.report) {
                            #[cfg(target_os = "macos")]
                            std::process::Command::new("open")
                                .arg("-R")
                                .arg(&path)
                                .spawn()
                                .ok();
                        }
                    }),
                ),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .child(toolbar)
            .child(
                div()
                    .id("report-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(18.0))
                    .text_size(px(13.0))
                    .child(
                        rever_ui_markdown::markdown::markdown(&report, &theme)
                            .id("report-markdown")
                            .lang(lang),
                    ),
            )
    }
}
