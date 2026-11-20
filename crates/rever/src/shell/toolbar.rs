//! The toolbar (navigation, address bar with the bookmark star, IP badge, "⋮" menu) and
//! the bookmark bar under it. The two read as one surface, as in App.tsx.

use gpui::{ClickEvent, Context, div, prelude::*, px};
use rever_ui_kit::theme::Theme;

use super::{icon, icon_button};
use crate::app::{Browser, is_web_url};

impl Browser {
    pub(super) fn toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = self.lang;
        let row = div()
            .h(px(46.0))
            .px(px(8.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .child(icon_button("back", "back", 32.0, theme).on_click(
                cx.listener(|this, _: &ClickEvent, _, _| this.engine.back(this.active_id())),
            ))
            .child(icon_button("forward", "forward", 32.0, theme).on_click(
                cx.listener(|this, _: &ClickEvent, _, _| this.engine.forward(this.active_id())),
            ))
            .child(icon_button("reload", "reload", 32.0, theme).on_click(
                cx.listener(|this, _: &ClickEvent, _, _| this.engine.reload(this.active_id())),
            ))
            .child(
                // `.omnibox`, with room on the right for the star inside the pill.
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h(px(34.0))
                    .mx(px(4.0))
                    .pl(px(16.0))
                    .pr(px(36.0))
                    .flex()
                    .items_center()
                    .rounded(px(17.0))
                    .bg(theme.chrome_omnibox)
                    .text_size(px(14.0))
                    .text_color(theme.text)
                    .child(self.views.address.clone())
                    .child(self.bookmark_star(theme, cx)),
            )
            .child(
                div()
                    .h(px(28.0))
                    .px(px(10.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .rounded(px(14.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .child(lang.t("proxy.label")),
            )
            .child(self.ip_badge.clone())
            .child(icon_button("profile", "profile", 32.0, theme))
            .child(icon_button("toggle-chat", "panel-right", 32.0, theme).on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_chat(cx)),
            ))
            .child(self.menu.clone());
        div()
            .flex_none()
            .bg(theme.chrome_toolbar)
            .border_b_1()
            .border_color(theme.border)
            .child(row)
            .child(self.views.bookmark_bar.clone())
    }

    /// The star toggles a bookmark for the active page; only http(s) pages can have one.
    fn bookmark_star(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let url = &self.tabs[self.active].url;
        let enabled = is_web_url(url);
        let marks = self.stores.bookmarks.read(cx).bookmarks();
        let bookmarked = marks.iter().any(|mark| mark.url == *url);
        let (name, color) = if bookmarked {
            ("star-filled", theme.accent)
        } else {
            ("star", theme.text_2)
        };
        let hover = theme.chrome_hover;
        // `.chrome-icon-btn.small`, 3px from the pill's right edge and centered in its 34px.
        div()
            .id("bookmark-star")
            .absolute()
            .right(px(3.0))
            .top(px(3.0))
            .size(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .when(!enabled, |star| star.opacity(0.4))
            .when(enabled, |star| {
                star.cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_bookmark(cx)))
            })
            .child(icon(name, 16.0, color))
    }
}
