//! The tab strip: one tab per page and the "+" button. It is also the window's title
//! bar, as in Chrome: the traffic lights sit at its left end and its empty part drags
//! the window.

use gpui::{ClickEvent, Context, SharedString, Window, div, img, prelude::*, px};
use rever_ui_kit::theme::Theme;

use super::{icon, icon_button};
use crate::app::Browser;

/// Height of the strip; `main.rs` centers the traffic lights in it.
pub(crate) const STRIP_HEIGHT: f32 = 40.0;
/// Room the three traffic lights take at the left end.
const TRAFFIC_LIGHTS_WIDTH: f32 = 78.0;

impl Browser {
    pub(super) fn tab_strip(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let lang = self.lang;
        let closable = self.tabs.len() > 1;
        // Full screen hides the traffic lights, so their room goes too.
        let left = if window.is_fullscreen() {
            8.0
        } else {
            TRAFFIC_LIGHTS_WIDTH
        };
        div()
            .h(px(STRIP_HEIGHT))
            .flex()
            .flex_none()
            .items_end()
            .gap(px(2.0))
            .pl(px(left))
            .pr(px(8.0))
            .bg(theme.chrome_frame)
            .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                let active = index == self.active;
                let (hover, dim) = (theme.chrome_hover, theme.text_dim);
                let label: SharedString = if tab.title.is_empty() {
                    lang.t("tab.new").into()
                } else {
                    tab.title.clone()
                };
                div()
                    .id(("tab", index))
                    .h(px(34.0))
                    .min_w(px(120.0))
                    .max_w(px(240.0))
                    .pl(px(12.0))
                    .pr(px(8.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .rounded_t(px(8.0))
                    .text_size(px(12.0))
                    .cursor_pointer()
                    .when(active, |tab| {
                        tab.bg(theme.chrome_toolbar).text_color(theme.text)
                    })
                    .when(!active, |tab| {
                        tab.text_color(theme.text_2)
                            .hover(move |style| style.bg(hover))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select_tab(index, window, cx)
                    }))
                    .child(match tab.favicon.clone() {
                        Some(favicon) => img(favicon).size(px(16.0)).flex_none().into_any_element(),
                        None => icon("globe", 16.0, dim).into_any_element(),
                    })
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .when(closable, |tab| {
                        tab.child(
                            div()
                                .id(("tab-close", index))
                                .size(px(18.0))
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .hover(move |style| style.bg(hover))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(index, window, cx);
                                }))
                                .child(icon("close", 11.0, dim)),
                        )
                    })
            }))
            .child(div().pb(px(3.0)).pl(px(4.0)).child(
                icon_button("new-tab", "plus", 28.0, theme).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.new_tab(window, cx)),
                ),
            ))
            // The empty rest of the strip: a double click zooms the window, as a title bar's does.
            .child(div().id("tab-strip-empty").flex_1().h_full().on_click(
                |event: &ClickEvent, window, _| {
                    if event.click_count() == 2 {
                        window.titlebar_double_click();
                    }
                },
            ))
    }
}
