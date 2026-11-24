//! How the window is drawn. Layout follows the Electron app's `src/renderer/src/App.tsx`:
//! the tab strip across the top; under it the toolbar with the bookmark bar, the page,
//! the bottom panel and the chip bar on the left, and the chat panel on the right.

mod chat;
mod chip_bar;
mod page;
mod redteam;
pub(crate) mod report;
pub(crate) mod shortcuts;
mod spaces;
mod tab_strip;

pub(crate) use tab_strip::STRIP_HEIGHT;
mod toolbar;

use std::borrow::Cow;

use gpui::{
    AssetSource, Context, Div, Rgba, SharedString, Stateful, Window, div, prelude::*, px, svg,
};
use rever_ui_kit::theme::Theme;

pub(crate) use self::chat::{CHAT_MIN_WIDTH, CHAT_WIDTH};
use self::chat::{CHAT_SLIDE, chat_panel_width, slide_width};
use crate::app::{Browser, Layout};

/// The bookmark star of App.tsx's address bar.
const STAR: &str = "12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2";

/// A globe: a circle, one meridian and the equator.
const GLOBE: &str = "<circle cx=\"12\" cy=\"12\" r=\"10\"/><path d=\"M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20\"/><path d=\"M2 12h20\"/>";

/// The kit's icons plus the bookmark star, which the kit does not carry.
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let (fill, shape) = match path {
            "icons/star.svg" => ("none", format!("<polygon points=\"{STAR}\"/>")),
            "icons/star-filled.svg" => ("black", format!("<polygon points=\"{STAR}\"/>")),
            // The tab of a page without an icon, as in Chrome.
            "icons/globe.svg" => ("none", GLOBE.to_owned()),
            "icons/arrow-down.svg" => ("none", "<path d=\"M12 5v14M19 12l-7 7-7-7\"/>".to_owned()),
            // The chat panel's controls: fold it away, let it cover the page, bring it back.
            "icons/panel-right.svg" => (
                "none",
                "<rect x=\"3\" y=\"3\" width=\"18\" height=\"18\" rx=\"2\"/><path d=\"M15 3v18\"/>"
                    .to_owned(),
            ),
            "icons/chevron-right.svg" => ("none", "<path d=\"m9 18 6-6-6-6\"/>".to_owned()),
            "icons/chevron-left.svg" => ("none", "<path d=\"m15 18-6-6 6-6\"/>".to_owned()),
            "icons/expand.svg" => (
                "none",
                "<path d=\"M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7\"/>".to_owned(),
            ),
            "icons/shrink.svg" => (
                "none",
                "<path d=\"M4 14h6v6M20 10h-6V4M14 10l7-7M3 21l7-7\"/>".to_owned(),
            ),
            // Three quarters of a circle: turned, it is the chat's "working" spinner.
            // Two sheets, one behind the other: copy a message.
            "icons/copy.svg" => (
                "none",
                "<rect width=\"14\" height=\"14\" x=\"8\" y=\"8\" rx=\"2\"/>\
                 <path d=\"M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2\"/>"
                    .to_owned(),
            ),
            // Four squares: the grid view of the spaces.
            "icons/grid.svg" => (
                "none",
                "<rect x=\"3\" y=\"3\" width=\"7\" height=\"7\" rx=\"1\"/>\
                 <rect x=\"14\" y=\"3\" width=\"7\" height=\"7\" rx=\"1\"/>\
                 <rect x=\"3\" y=\"14\" width=\"7\" height=\"7\" rx=\"1\"/>\
                 <rect x=\"14\" y=\"14\" width=\"7\" height=\"7\" rx=\"1\"/>"
                    .to_owned(),
            ),
            "icons/loader.svg" => (
                "none",
                "<path d=\"M21 12a9 9 0 1 1-6.219-8.56\"/>".to_owned(),
            ),
            _ => return rever_ui_kit::icons::Assets.load(path),
        };
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"{fill}\" \
             stroke=\"black\" stroke-width=\"2\" stroke-linecap=\"round\" \
             stroke-linejoin=\"round\">{shape}</svg>"
        );
        Ok(Some(Cow::Owned(svg.into_bytes())))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        rever_ui_kit::icons::Assets.list(path)
    }
}

fn icon(name: &str, size: f32, color: Rgba) -> impl IntoElement {
    svg()
        .path(format!("icons/{name}.svg"))
        .size(px(size))
        .flex_none()
        .text_color(color)
}

/// `.chrome-icon-btn`: a round, borderless button holding one icon.
fn icon_button(
    id: &'static str,
    name: &'static str,
    diameter: f32,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.chrome_hover;
    div()
        .id(id)
        .size(px(diameter))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(icon(name, diameter * 0.56, theme.text_2))
}

impl Render for Browser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.theme_mode, window.appearance());
        // The grid view of the spaces takes the whole window, as its own screen.
        if self.spaces_open {
            let root = div()
                .size_full()
                .relative()
                .text_color(theme.text)
                .text_size(px(13.0))
                .font_family(".SystemUIFont")
                .child(self.spaces_overview(&theme, window, cx))
                // The scan's dialog stays over the grid: its stats, log and
                // report buttons are how the scan is watched and stopped.
                .children(self.redteam_dialog(&theme, cx));
            return self.shortcut_handlers(root, cx);
        }
        // Opening and closing the bottom panel slides to the new height, as the chat does.
        let panel_target = if self.open_panel.is_some() {
            self.panel_height
        } else {
            0.0
        };
        let panel_height = match self.panel_slide {
            Some((started, from)) if started.elapsed() < CHAT_SLIDE => {
                window.request_animation_frame();
                let progress = started.elapsed().as_secs_f32() / CHAT_SLIDE.as_secs_f32();
                slide_width(from, panel_target, progress)
            }
            _ => {
                self.panel_slide = None;
                panel_target
            }
        };
        let panel_sliding = self.panel_slide.is_some();
        self.panel_drawn = panel_height;
        self.panel_last = self.open_panel.or(self.panel_last);
        // A panel that is sliding shut is drawn until it is gone.
        let shown_panel = self.panel_last.filter(|_| panel_height > 0.0);
        // The box App.tsx positions the bottom panel, the chip bar and the detail drawer in.
        let page_box = div()
            .flex_1()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            .child(self.page(&theme, cx))
            .when_some(shown_panel, |column, panel| {
                // Drawn at its full height and clipped while it slides, like a drawer:
                // its rows do not squeeze at every frame.
                column.child(
                    div()
                        .h(px(panel_height))
                        .flex_none()
                        // Only while it slides: the resize handle reaches past the top edge.
                        .when(panel_sliding, |clip| clip.overflow_hidden())
                        .child(self.panel(panel, &theme, cx)),
                )
            })
            .child(self.chip_bar(&theme, cx))
            .child(self.views.drawer.clone());
        // The chat panel takes what the user gave it, as far as the window allows.
        let window_width: f32 = window.viewport_size().width.into();
        // A sidebar takes its width from what the page and the chat share.
        let layout = self.layout;
        let window_width = if layout.has_sidebar() {
            window_width - tab_strip::SIDEBAR_WIDTH
        } else {
            window_width
        };
        let target = chat_panel_width(
            window_width,
            self.chat_width,
            self.chat_collapsed,
            self.chat_expanded,
        );
        // Folding, opening and expanding slide to the new width instead of jumping.
        let (chat_width, sliding_from) = match self.chat_slide {
            Some((started, from)) if started.elapsed() < CHAT_SLIDE => {
                window.request_animation_frame();
                let progress = started.elapsed().as_secs_f32() / CHAT_SLIDE.as_secs_f32();
                (slide_width(from, target, progress), Some(from))
            }
            _ => {
                self.chat_slide = None;
                (target, None)
            }
        };
        self.chat_drawn = chat_width;
        // While it slides shut or open the panel keeps its open width and is clipped, so
        // its text does not rewrap at every frame; it moves like a drawer.
        let panel_width = match sliding_from {
            Some(from) if from == 0.0 || target == 0.0 => from.max(target),
            _ => chat_width,
        };
        // The two columns are placed by their edges, not as items of a flex row: a row
        // first measures each item at its narrowest, and GPUI keeps a text cut to an
        // ellipsis at that width (the history rows came out as "…").
        let left = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .right(px(chat_width))
            .flex()
            .flex_col()
            // With everything in the sidebar only the bookmark bar stays over the page.
            .child(if layout == Layout::Sidebar {
                div()
                    .flex_none()
                    .bg(theme.chrome_toolbar)
                    .child(self.views.bookmark_bar.clone())
                    .into_any_element()
            } else {
                self.toolbar(&theme, cx).into_any_element()
            })
            .child(page_box);
        let chat = div()
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(chat_width))
            // Clipped only while it slides: the handle that resizes the panel reaches
            // over its left edge, and clipped it was too thin to take hold of.
            .when(sliding_from.is_some(), |chat| chat.overflow_hidden())
            .child(self.chat_panel(&theme, panel_width, cx));
        // The handle on the border between the page and the chat: folds the chat away,
        // and stays at the window's edge to bring it back.
        let folded = self.chat_collapsed;
        let handle = div()
            .id("chat-fold")
            .absolute()
            .top(gpui::relative(0.5))
            .mt(px(-28.0))
            .right(px((chat_width - 9.0).max(0.0)))
            .w(px(18.0))
            .h(px(56.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(9.0))
            .border_1()
            .border_color(theme.border_2)
            .bg(theme.chrome_toolbar)
            .shadow_md()
            .cursor_pointer()
            .occlude()
            .hover(|style| style.bg(theme.chrome_hover))
            .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| this.toggle_chat(cx)))
            .child(icon(
                if folded {
                    "chevron-left"
                } else {
                    "chevron-right"
                },
                12.0,
                theme.text_2,
            ));
        // A chat that covers the page has no border to put the handle on.
        let covers_page = chat_width >= window_width;
        let root = div()
            .size_full()
            .flex()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.0))
            .font_family(".SystemUIFont");
        // The tabs: a strip over everything, or a sidebar beside it.
        let root = if layout.has_sidebar() {
            root.child(self.sidebar(&theme, window, cx))
        } else {
            root.flex_col().child(self.tab_strip(&theme, window, cx))
        };
        let root = root
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .when(layout.has_sidebar(), |area| area.h_full())
                    .relative()
                    // A chat that covers the page leaves the page column out; a folded
                    // chat leaves itself out.
                    .when(!covers_page, |area| area.child(left))
                    .when(chat_width > 0.0, |area| area.child(chat))
                    .when(!covers_page, |area| area.child(handle))
                    .children(self.redteam_dialog(&theme, cx)),
            )
            // Over everything: the agent's question whether a tool may run.
            .child(self.permission_prompt.clone());
        self.shortcut_handlers(root, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 별_아이콘은_앱이_주고_나머지는_키트가_준다() {
        let outline = Assets.load("icons/star.svg").unwrap().unwrap();
        assert!(String::from_utf8_lossy(&outline).contains("fill=\"none\""));
        let filled = Assets.load("icons/star-filled.svg").unwrap().unwrap();
        assert!(String::from_utf8_lossy(&filled).contains("fill=\"black\""));
        assert!(Assets.load("icons/globe.svg").unwrap().is_some());
        assert!(Assets.load("icons/traffic.svg").unwrap().is_some());
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }
}
