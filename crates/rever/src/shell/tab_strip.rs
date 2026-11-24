//! The tabs: one per page and the "+" button, as a strip across the top or as a list
//! in a sidebar. Either one is also the window's title bar, as in Chrome: the traffic
//! lights sit at its left end and its empty part drags the window.

use gpui::{
    AnyElement, ClickEvent, Context, Entity, FontWeight, MouseButton, MouseDownEvent, SharedString,
    Window, div, img, prelude::*, px,
};
use rever_ui_kit::theme::Theme;

use super::{icon, icon_button};
use crate::app::{Browser, Layout};
use crate::engine::TabId;
use crate::memory::{megabytes, page_bytes};

/// Height of the strip; `main.rs` centers the traffic lights in it.
pub(crate) const STRIP_HEIGHT: f32 = 40.0;
/// Room the three traffic lights take at the left end.
const TRAFFIC_LIGHTS_WIDTH: f32 = 78.0;

/// Width of the sidebar the tabs are listed in.
pub(crate) const SIDEBAR_WIDTH: f32 = 240.0;

impl Browser {
    pub(super) fn tab_strip(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Full screen hides the traffic lights, so their room goes too.
        let left = if window.is_fullscreen() {
            8.0
        } else {
            TRAFFIC_LIGHTS_WIDTH
        };
        let tabs: Vec<_> = self
            .space_tabs()
            .into_iter()
            .map(|index| self.tab(index, false, theme, cx))
            .collect();
        div()
            .h(px(STRIP_HEIGHT))
            .flex()
            .flex_none()
            .items_end()
            .gap(px(2.0))
            .pl(px(left))
            .pr(px(8.0))
            .bg(theme.chrome_frame)
            // The tabs of the space that is shown.
            .children(tabs)
            .child(div().pb(px(3.0)).pl(px(4.0)).child(
                icon_button("new-tab", "plus", 28.0, theme).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.new_tab(window, cx)),
                ),
            ))
            // The empty rest of the strip: a double click zooms the window, as a title bar's does.
            .child(title_bar_room("tab-strip-empty"))
            // The way to the grid view of the spaces.
            .child(div().pb(px(3.0)).child(self.spaces_button(theme, cx)))
    }

    /// The sidebar of the two layouts that list the tabs at the side. In
    /// [`Layout::Sidebar`] it holds the toolbar's parts as well.
    pub(super) fn sidebar(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let everything = self.layout == Layout::Sidebar;
        let left = if window.is_fullscreen() {
            8.0
        } else {
            TRAFFIC_LIGHTS_WIDTH
        };
        let tabs: Vec<_> = self
            .space_tabs()
            .into_iter()
            .map(|index| self.tab(index, true, theme, cx))
            .collect();
        let (hover, dim) = (theme.chrome_hover, theme.text_dim);
        let new_tab = div()
            .id("new-tab")
            .h(px(34.0))
            .px(px(12.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .rounded(px(8.0))
            .text_size(px(12.0))
            .text_color(theme.text_2)
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.new_tab(window, cx)))
            .child(icon("plus", 14.0, dim))
            .child(self.lang.t("tab.new"));
        let mut bar = div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .bg(theme.chrome_frame)
            .border_r_1()
            .border_color(theme.border)
            // The traffic lights sit in this row, which is the title bar here.
            .child(
                div()
                    .h(px(STRIP_HEIGHT))
                    .flex()
                    .flex_none()
                    .items_center()
                    .pl(px(left))
                    .pr(px(8.0))
                    .child(title_bar_room("sidebar-empty"))
                    .child(self.spaces_button(theme, cx)),
            );
        if everything {
            bar = bar
                .child(
                    div()
                        .px(px(8.0))
                        .flex()
                        .flex_none()
                        .items_center()
                        .child(self.nav_buttons(theme, cx))
                        .child(div().flex_1())
                        .child(self.menu.clone()),
                )
                .child(
                    div()
                        .px(px(8.0))
                        .pt(px(4.0))
                        .pb(px(8.0))
                        .flex_none()
                        .child(self.omnibox(theme, cx)),
                );
        }
        bar = bar.child(
            div()
                .id("side-tabs")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(8.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .children(tabs)
                .child(new_tab),
        );
        if everything {
            bar = bar.child(
                div()
                    .h(px(STRIP_HEIGHT))
                    .px(px(8.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(self.toolbar_tail(theme)),
            );
        }
        bar
    }

    /// The way to the grid view of the spaces.
    fn spaces_button(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        icon_button("spaces", "grid", 28.0, theme).on_click(
            cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_spaces(window, cx)),
        )
    }

    /// One tab: `side` draws it as a row of the sidebar, otherwise as a tab of the strip.
    fn tab(&self, index: usize, side: bool, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let tab = &self.tabs[index];
        let active = index == self.active;
        let closable = self.tabs.len() > 1;
        let (browser, id) = (cx.entity(), tab.id);
        let (hover, dim) = (theme.chrome_hover, theme.text_dim);
        let label: SharedString = if tab.title.is_empty() {
            self.lang.t("tab.new").into()
        } else {
            tab.title.clone()
        };
        div()
            .id(("tab", index))
            // A sleeping tab is drawn faint: its page loads again when it is picked.
            .when(tab.asleep, |tab| tab.opacity(0.55))
            .h(px(34.0))
            .pl(px(12.0))
            .pr(px(8.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .text_size(px(12.0))
            .cursor_pointer()
            .when(side, |tab| tab.rounded(px(8.0)))
            .when(!side, |tab| {
                tab.min_w(px(120.0)).max_w(px(240.0)).rounded_t(px(8.0))
            })
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
            // The wheel button closes the tab, as in Chrome.
            .when(closable, |tab| {
                tab.on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        this.close_tab(index, window, cx)
                    }),
                )
            })
            .on_hover(cx.listener(|this, hovered: &bool, _, _| {
                if *hovered {
                    this.measure_page_memory();
                }
            }))
            .tooltip(move |_, cx| {
                let browser = browser.clone();
                cx.new(|_| TabCard { browser, tab: id }).into()
            })
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
            .into_any_element()
    }
}

/// The empty part of the row the traffic lights sit in: a double click zooms the
/// window, as a title bar's does.
fn title_bar_room(id: &'static str) -> impl IntoElement {
    div()
        .id(id)
        .flex_1()
        .h_full()
        .on_click(|event: &ClickEvent, window, _| {
            if event.click_count() == 2 {
                window.titlebar_double_click();
            }
        })
}

/// The card a hovered tab shows, as Chrome's does: the page's title, its site, and
/// the memory it holds. It reads the browser on every draw, so the memory line
/// appears when the measurement the hover asked for arrives.
struct TabCard {
    browser: Entity<Browser>,
    tab: TabId,
}

impl Render for TabCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let browser = self.browser.read(cx);
        let theme = Theme::resolve(browser.theme_mode, window.appearance());
        let card = div()
            .w(px(260.0))
            .p(px(10.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .rounded(px(8.0))
            .bg(theme.bg)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .text_size(px(12.0));
        let Some(tab) = browser.tabs.iter().find(|tab| tab.id == self.tab) else {
            return card;
        };
        let title: SharedString = if tab.title.is_empty() {
            browser.lang.t("tab.new").into()
        } else {
            tab.title.clone()
        };
        let site = url::Url::parse(&tab.url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| tab.url.clone());
        // A sleeping tab has no page: an older report must not speak for it.
        let memory = (!tab.asleep)
            .then(|| page_bytes(&browser.page_memory, &tab.url))
            .flatten();
        card.child(
            div()
                .text_color(theme.text)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(div().text_color(theme.text_dim).truncate().child(site))
        .children(memory.map(|bytes| {
            div()
                .mt(px(4.0))
                .pt(px(6.0))
                .border_t_1()
                .border_color(theme.border_2)
                .text_color(theme.text_2)
                .child(format!(
                    "{}: {}",
                    browser.lang.t("tab.memory"),
                    megabytes(bytes)
                ))
        }))
    }
}
