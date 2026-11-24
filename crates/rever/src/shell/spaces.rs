//! The grid view of the spaces: every space as a tile with a picture of its page, in
//! place of the browser. A click on a tile shows that space; the last tile starts a
//! new one. Opening it, the page shrinks into its tile; leaving it, the chosen tile
//! grows to fill the window.

use gpui::{ClickEvent, Context, ObjectFit, Window, div, img, prelude::*, px};
use rever_ui_kit::theme::Theme;
use rever_ui_state::spaces::grid_columns;

use super::chat::slide_width;
use super::{STRIP_HEIGHT, icon, icon_button};
use crate::app::{Browser, SPACES_ZOOM};

/// The room around the grid and between its tiles.
const MARGIN: f32 = 48.0;
const GAP: f32 = 28.0;
const PAD_TOP: f32 = 24.0;
/// A tile is as wide as the row allows and this much as high: a window's shape.
const TILE_SHAPE: f32 = 0.64;
const TILE_RADIUS: f32 = 14.0;
/// The line under a picture, with the space's name, and the room above it.
const LABEL_HEIGHT: f32 = 20.0;
const LABEL_GAP: f32 = 10.0;

/// The key context of the grid view: Escape leaves it.
const SPACES_CONTEXT: &str = "Spaces";

/// A rectangle in the window: left, top, width, height.
type Place = (f32, f32, f32, f32);

/// How wide a tile is when `tiles` of them share a window `width` wide.
fn tile_width(width: f32, tiles: usize) -> f32 {
    let columns = grid_columns(tiles) as f32;
    ((width - 2.0 * MARGIN - GAP * (columns - 1.0)) / columns).max(160.0)
}

/// Where the picture of the tile at `index` sits, with the grid scrolled to its top.
/// The zoom moves a page's picture to and from this place.
fn tile_place(index: usize, tiles: usize, width: f32) -> Place {
    let columns = grid_columns(tiles);
    let tile = tile_width(width, tiles);
    let height = tile * TILE_SHAPE;
    let (column, row) = ((index % columns) as f32, (index / columns) as f32);
    (
        MARGIN + column * (tile + GAP),
        STRIP_HEIGHT + PAD_TOP + row * (height + LABEL_GAP + LABEL_HEIGHT + GAP),
        tile,
        height,
    )
}

/// The place `progress` (0..1) of the way from `from` to `to`, settling at the end.
fn between(from: Place, to: Place, progress: f32) -> Place {
    (
        slide_width(from.0, to.0, progress),
        slide_width(from.1, to.1, progress),
        slide_width(from.2, to.2, progress),
        slide_width(from.3, to.3, progress),
    )
}

impl Browser {
    pub(super) fn spaces_overview(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let lang = self.lang;
        let closable = self.spaces.len() > 1;
        // The tiles and the "new space" tile after them.
        let tiles_count = self.spaces.len() + 1;
        let window_size = window.viewport_size();
        let window_width: f32 = window_size.width.into();
        let width = tile_width(window_width, tiles_count);
        let height = width * TILE_SHAPE;
        let (hover, dim) = (theme.chrome_hover, theme.text_dim);

        // The zoom: a space's picture on its way between the page and its tile, and
        // the grid fading in or out around it.
        let zoom = self.spaces_zoom.map(|(started, space, opening)| {
            let progress = (started.elapsed().as_secs_f32() / SPACES_ZOOM.as_secs_f32()).min(1.0);
            (space, opening, progress)
        });
        let moving = zoom.filter(|(_, opening, progress)| !*opening || *progress < 1.0);
        if moving.is_some_and(|(_, _, progress)| progress < 1.0) {
            window.request_animation_frame();
        }
        let faded = match moving {
            Some((_, true, progress)) => slide_width(0.0, 1.0, progress),
            Some((_, false, progress)) => slide_width(1.0, 0.0, progress),
            None => 1.0,
        };
        let flying = moving.and_then(|(space, opening, progress)| {
            let index = self.spaces.iter().position(|entry| entry.id == space)?;
            let shot = self
                .space_shots
                .iter()
                .find(|(of, _)| *of == space)
                .map(|(_, shot)| shot.clone())?;
            // Where the page is in the browser; the whole window before it was laid out.
            let page = self.page_bounds.get();
            let page: Place = if f32::from(page.size.width) > 0.0 {
                (
                    page.origin.x.into(),
                    page.origin.y.into(),
                    page.size.width.into(),
                    page.size.height.into(),
                )
            } else {
                (0.0, 0.0, window_width, window_size.height.into())
            };
            let tile = tile_place(index, tiles_count, window_width);
            let (from, to) = if opening { (page, tile) } else { (tile, page) };
            let (left, top, wide, high) = between(from, to, progress);
            let radius = if opening {
                slide_width(0.0, TILE_RADIUS, progress)
            } else {
                slide_width(TILE_RADIUS, 0.0, progress)
            };
            Some((
                space,
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(wide))
                    .h(px(high))
                    .rounded(px(radius))
                    .overflow_hidden()
                    .shadow_lg()
                    .child(img(shot).size_full().object_fit(ObjectFit::Cover)),
            ))
        });
        let flying_space = flying.as_ref().map(|(space, _)| *space);

        let tiles = self.spaces.iter().map(|space| {
            let id = space.id;
            let current = id == self.space;
            let tabs: Vec<_> = self.tabs.iter().filter(|tab| tab.space == id).collect();
            let shot = self
                .space_shots
                .iter()
                .find(|(of, _)| *of == id)
                .map(|(_, shot)| shot.clone());
            // Without a picture (the page sleeps), the tile names the page.
            let title = tabs
                .iter()
                .find(|tab| Some(tab.id) == space.shown)
                .or(tabs.first())
                .map(|tab| tab.title.clone())
                .unwrap_or_default();
            let picture = div()
                .id(("space", id.0 as usize))
                .group("space-tile")
                .relative()
                .h(px(height))
                .rounded(px(TILE_RADIUS))
                .overflow_hidden()
                .bg(theme.bg)
                .cursor_pointer()
                // The space that is shown stands out, as the selected tab does.
                .when(current, |tile| tile.border_2().border_color(theme.accent))
                .when(!current, |tile| {
                    tile.border_1().border_color(theme.border_2)
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.leave_spaces(id, window, cx)
                }))
                .child(match shot {
                    Some(shot) => img(shot)
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                        // Its picture is the one in flight.
                        .when(flying_space == Some(id), |picture| picture.opacity(0.0))
                        .into_any_element(),
                    None => div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(px(24.0))
                        .text_color(dim)
                        .child(div().truncate().child(title))
                        .into_any_element(),
                })
                .when(closable, |tile| {
                    tile.child(
                        div()
                            .id(("space-close", id.0 as usize))
                            .absolute()
                            .top(px(8.0))
                            .right(px(8.0))
                            .size(px(24.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(theme.chrome_toolbar)
                            .invisible()
                            .group_hover("space-tile", |button| button.visible())
                            .hover(move |button| button.bg(hover))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.close_space(id, window, cx);
                            }))
                            .child(icon("close", 12.0, dim)),
                    )
                });
            div()
                .w(px(width))
                .flex()
                .flex_col()
                .gap(px(LABEL_GAP))
                .child(picture)
                .child(
                    div()
                        .h(px(LABEL_HEIGHT))
                        .px(px(6.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(12.0))
                        .child(div().min_w_0().truncate().child(space.name.clone()))
                        .child(
                            div().flex_none().text_color(dim).child(
                                lang.t("spaces.tabs")
                                    .replace("{n}", &tabs.len().to_string()),
                            ),
                        ),
                )
        });

        let new_space = div()
            .id("space-new")
            .w(px(width))
            .h(px(height))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(TILE_RADIUS))
            .border_1()
            .border_color(theme.border_2)
            .cursor_pointer()
            .hover(move |tile| tile.bg(hover))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.close_spaces(window);
                this.new_space(window, cx);
            }))
            .child(icon("plus", 34.0, dim));

        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .key_context(SPACES_CONTEXT)
            .track_focus(&self.spaces_focus)
            .bg(theme.chrome_frame)
            .child(
                // Where the tab strip is: the count of the spaces, and the way back.
                div()
                    .relative()
                    .h(px(STRIP_HEIGHT))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .opacity(faded)
                    .text_color(theme.text_2)
                    .child(
                        lang.t("spaces.count")
                            .replace("{n}", &self.spaces.len().to_string()),
                    )
                    .child(div().absolute().right(px(8.0)).top(px(5.0)).child(
                        icon_button("spaces-close", "grid", 28.0, theme).on_click(cx.listener(
                            |this, _: &ClickEvent, window, cx| this.toggle_spaces(window, cx),
                        )),
                    )),
            )
            .child(
                div()
                    .id("spaces-grid")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(MARGIN))
                    .py(px(PAD_TOP))
                    .flex()
                    .flex_wrap()
                    .content_start()
                    .gap(px(GAP))
                    .opacity(faded)
                    .children(tiles)
                    .child(new_space),
            )
            .children(flying.map(|(_, picture)| picture))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 타일_둘은_창_너비를_둘로_나눠_쓴다() {
        // 1,440에서 양쪽 여백 96과 사이 28을 뺀 나머지의 절반.
        assert_eq!(tile_width(1440.0, 2), 658.0);
    }

    #[test]
    fn 타일이_많으면_한_줄에_셋을_놓는다() {
        let three = tile_width(1440.0, 6);
        assert!(three * 3.0 + GAP * 2.0 + MARGIN * 2.0 <= 1440.0);
    }

    #[test]
    fn 좁은_창에서도_타일은_알아볼_만큼의_너비를_지킨다() {
        assert_eq!(tile_width(200.0, 3), 160.0);
    }

    #[test]
    fn 첫_타일의_그림은_여백_안쪽_탭_줄_아래에서_시작한다() {
        let (left, top, ..) = tile_place(0, 2, 1440.0);
        assert_eq!((left, top), (MARGIN, STRIP_HEIGHT + PAD_TOP));
    }

    #[test]
    fn 둘째_타일은_첫_타일_옆에_간격만큼_떨어져_놓인다() {
        let (first, _, width, _) = tile_place(0, 2, 1440.0);
        assert_eq!(tile_place(1, 2, 1440.0).0, first + width + GAP);
    }

    #[test]
    fn 다음_줄의_타일은_그림과_이름_줄만큼_아래에_놓인다() {
        // 타일 셋이면 두 칸: 셋째가 둘째 줄의 첫 칸이다.
        let (left, top, _, height) = tile_place(0, 3, 1440.0);
        assert_eq!(
            tile_place(2, 3, 1440.0),
            (
                left,
                top + height + LABEL_GAP + LABEL_HEIGHT + GAP,
                tile_width(1440.0, 3),
                height
            )
        );
    }

    #[test]
    fn 전환의_끝은_도착_자리와_같다() {
        let (from, to) = ((0.0, 0.0, 1000.0, 700.0), (48.0, 62.0, 658.0, 421.0));
        assert_eq!(between(from, to, 1.0), to);
    }

    #[test]
    fn 전환의_시작은_출발_자리와_같다() {
        let (from, to) = ((0.0, 0.0, 1000.0, 700.0), (48.0, 62.0, 658.0, 421.0));
        assert_eq!(between(from, to, 0.0), from);
    }
}
