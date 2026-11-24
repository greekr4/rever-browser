//! The toolbar (navigation, address bar with the bookmark star, IP badge, "⋮" menu) and
//! the bookmark bar under it. The two read as one surface, as in App.tsx.

use gpui::{ClickEvent, Context, div, prelude::*, px};
use rever_ui_kit::theme::Theme;

use super::{icon, icon_button};
use crate::app::{Browser, is_web_url};

impl Browser {
    pub(super) fn toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let row = div()
            .h(px(46.0))
            .px(px(8.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .child(self.nav_buttons(theme, cx))
            .child(self.omnibox(theme, cx).flex_1().mx(px(4.0)))
            .child(self.toolbar_tail(theme))
            .child(self.menu.clone());
        div()
            .flex_none()
            .bg(theme.chrome_toolbar)
            .border_b_1()
            .border_color(theme.border)
            .child(row)
            .child(self.views.bookmark_bar.clone())
    }

    /// Back, forward and reload.
    pub(super) fn nav_buttons(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
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
    }

    /// `.omnibox`: the address field, with room on the right for the star inside the pill.
    pub(super) fn omnibox(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .relative()
            .min_w_0()
            .h(px(34.0))
            .pl(px(16.0))
            .pr(px(36.0))
            .flex()
            .items_center()
            .rounded(px(17.0))
            .bg(theme.chrome_omnibox)
            .text_size(px(14.0))
            .text_color(theme.text)
            // A long address is cut at the pill's edge, not drawn over the star.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(self.views.address.clone()),
            )
            .child(self.bookmark_star(theme, cx))
    }

    /// The proxy label, the IP badge and the profile button.
    pub(super) fn toolbar_tail(&self, theme: &Theme) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
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
                    .child(self.lang.t("proxy.label")),
            )
            .child(self.ip_badge.clone())
            .child(icon_button("profile", "profile", 32.0, theme))
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
