//! How the window is drawn. Layout follows the Electron app's `src/renderer/src/App.tsx`:
//! the tab strip across the top; under it the toolbar with the bookmark bar, the page,
//! the bottom panel and the chip bar on the left, and the chat panel on the right.

mod chat;
mod chip_bar;
mod page;
mod tab_strip;
mod toolbar;

use std::borrow::Cow;

use gpui::{
    AssetSource, Context, Div, Rgba, SharedString, Stateful, Window, div, prelude::*, px, svg,
};
use rever_ui_kit::theme::Theme;

use self::chat::CHAT_WIDTH;
use crate::app::Browser;

/// The bookmark star of App.tsx's address bar.
const STAR: &str = "12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2";

/// The kit's icons plus the bookmark star, which the kit does not carry.
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let fill = match path {
            "icons/star.svg" => "none",
            "icons/star-filled.svg" => "black",
            _ => return rever_ui_kit::icons::Assets.load(path),
        };
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"{fill}\" \
             stroke=\"black\" stroke-width=\"2\" stroke-linecap=\"round\" \
             stroke-linejoin=\"round\"><polygon points=\"{STAR}\"/></svg>"
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
        // The box App.tsx positions the bottom panel, the chip bar and the detail drawer in.
        let page_box = div()
            .flex_1()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            .child(self.page(&theme, cx))
            .when_some(self.open_panel, |column, panel| {
                column.child(self.panel(panel, &theme, cx))
            })
            .child(self.chip_bar(&theme, cx))
            .child(self.views.drawer.clone());
        // The two columns are placed by their edges, not as items of a flex row: a row
        // first measures each item at its narrowest, and GPUI keeps a text cut to an
        // ellipsis at that width (the history rows came out as "…").
        let left = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .right(px(CHAT_WIDTH))
            .flex()
            .flex_col()
            .child(self.toolbar(&theme, cx))
            .child(page_box);
        let chat = div()
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .child(self.chat_panel(&theme, cx));
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.0))
            .font_family(".SystemUIFont")
            .child(self.tab_strip(&theme, cx))
            .child(div().flex_1().min_h_0().relative().child(left).child(chat))
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
        assert!(Assets.load("icons/traffic.svg").unwrap().is_some());
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }
}
