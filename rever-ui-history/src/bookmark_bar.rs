//! Port of `src/renderer/src/components/BookmarkBar.tsx`: the row of bookmark chips under the toolbar.
//!
//! Clicking a chip emits [`OpenUrl`] where the component called the navigation-request
//! store; the × that appears on a hovered chip removes the bookmark.

use gpui::{
    AnyElement, ClickEvent, Context, Empty, Entity, EventEmitter, Rgba, SharedString, TextRun,
    Window, div, prelude::*, px,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};

use crate::bookmarks::{Bookmark, BookmarksStore};
// Same favicon source as HistoryPanel — Google's s2 service resolves the
// site's real favicon by hostname, so the bar looks like Chrome's without
// having to capture page-favicon-updated events per tab.
use crate::history_panel::{favicon, single_line};
use crate::{LocalTokens, OpenUrl};

/// The chip's `title`: the full title and the URL, one per line.
pub fn chip_tooltip(bookmark: &Bookmark) -> String {
    format!("{}\n{}", bookmark.title, bookmark.url)
}

/// Which chip is hovered after chip `id` reports entering or leaving. A leave only
/// counts for the chip that is current: moving between two chips can report the enter
/// of the new one before the leave of the old.
pub fn hovered_after(current: Option<&str>, id: &str, entered: bool) -> Option<String> {
    if entered {
        Some(id.to_string())
    } else {
        current.filter(|current| *current != id).map(str::to_string)
    }
}

/// `max-width: 130px` on the chip's title.
pub const TITLE_MAX_WIDTH: f32 = 130.0;

/// The width to give a title that measures `natural` pixels: none while it fits, the
/// maximum once it does not. GPUI shapes a text once, at the width of its first
/// measurement, so it cuts with an ellipsis only inside a box of a set width; a
/// `max-width` alone would clip the title bare.
pub fn title_width(natural: f32) -> Option<f32> {
    (natural > TITLE_MAX_WIDTH).then_some(TITLE_MAX_WIDTH)
}

pub struct BookmarkBar {
    store: Entity<BookmarksStore>,
    /// Id of the bookmark under the pointer: `.bookmark-chip:hover .bookmark-chip-x`.
    hovered: Option<String>,
    lang: Lang,
    theme_mode: ThemeMode,
}

impl EventEmitter<OpenUrl> for BookmarkBar {}

impl BookmarkBar {
    pub fn new(
        store: Entity<BookmarksStore>,
        lang: Lang,
        theme_mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self {
            store,
            hovered: None,
            lang,
            theme_mode,
        }
    }
}

/// The `title` attribute: GPUI has no native tooltip, so this is a small box of its own.
struct Tooltip {
    text: SharedString,
    background: Rgba,
    border: Rgba,
    color: Rgba,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(self.border)
            .bg(self.background)
            .text_color(self.color)
            .text_size(px(11.0))
            .line_height(px(15.0))
            .font_family(".SystemUIFont")
            .children(
                self.text
                    .split('\n')
                    .map(|line| div().child(line.to_string())),
            )
    }
}

impl Render for BookmarkBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bookmarks = self.store.read(cx).bookmarks();
        if bookmarks.is_empty() {
            return Empty.into_any_element();
        }
        let theme = Theme::resolve(self.theme_mode, window.appearance());
        let local = LocalTokens::resolve(self.theme_mode, window.appearance());
        let remove_label = self.lang.t("bookmark.remove");
        let tooltip = move |text: SharedString| {
            move |_: &mut Window, cx: &mut gpui::App| {
                cx.new(|_| Tooltip {
                    text: text.clone(),
                    background: local.surface,
                    border: theme.border_2,
                    color: theme.text,
                })
                .into()
            }
        };

        // The chip's font: what the bar inherits, at the chip's 12px.
        let font = window.text_style().font();
        let natural_width = |title: &SharedString| {
            let run = TextRun {
                len: title.len(),
                font: font.clone(),
                color: theme.text_2.into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line = window
                .text_system()
                .shape_line(title.clone(), px(12.0), &[run], None);
            f32::from(line.width)
        };

        let chips: Vec<AnyElement> = bookmarks
            .iter()
            .enumerate()
            .map(|(index, bookmark)| {
                let hovered = self.hovered.as_deref() == Some(bookmark.id.as_str());
                let (hover_id, remove_id) = (bookmark.id.clone(), bookmark.id.clone());
                let url = bookmark.url.clone();
                let title: SharedString = single_line(&bookmark.title).into();
                let width = title_width(natural_width(&title));
                let (chip_hover, x_hover, x_hover_text) =
                    (local.glass_hover, theme.border_2, theme.text);
                // `.bookmark-chip`
                div()
                    // Keyed by the bookmark, not only by its place: GPUI keeps "was
                    // hovered" per element id, so the chip that slides under the pointer
                    // when its neighbour is removed would inherit it and never report
                    // the hover that shows its ×.
                    .id((SharedString::from(bookmark.id.clone()), index))
                    .debug_selector(|| format!("bookmark-chip-{index}"))
                    .h(px(24.0))
                    .px(px(8.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(6.0))
                    .text_size(px(12.0))
                    .line_height(px(16.0))
                    .text_color(theme.text_2)
                    .cursor_pointer()
                    .hover(move |style| style.bg(chip_hover))
                    .on_hover(cx.listener(move |this, entered: &bool, _, cx| {
                        let next = hovered_after(this.hovered.as_deref(), &hover_id, *entered);
                        if next != this.hovered {
                            this.hovered = next;
                            cx.notify();
                        }
                    }))
                    .tooltip(tooltip(chip_tooltip(bookmark).into()))
                    .on_click(
                        cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(OpenUrl(url.clone()))),
                    )
                    .child(favicon(&bookmark.url, theme.surface_3))
                    .child(
                        // The set width goes one level below the chip's flex item: a flex
                        // item is first measured for its content size, ignoring its own.
                        div().flex_none().child(
                            div()
                                .when_some(width, |title, width| title.w(px(width)))
                                .truncate()
                                .child(title),
                        ),
                    )
                    .when(hovered, |chip| {
                        chip.child(
                            // `.bookmark-chip-x`
                            div()
                                .id(("bookmark-chip-x", index))
                                .debug_selector(|| format!("bookmark-chip-x-{index}"))
                                .size(px(14.0))
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .rounded(px(3.0))
                                .text_size(px(13.0))
                                .line_height(px(14.0))
                                .text_color(theme.text_dim)
                                .hover(move |style| style.bg(x_hover).text_color(x_hover_text))
                                .tooltip(tooltip(remove_label.into()))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    // The chip under the × must not open as well.
                                    cx.stop_propagation();
                                    this.store.update(cx, |store, cx| {
                                        store.remove(&remove_id);
                                        cx.notify();
                                    });
                                }))
                                .child("×"),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        div()
            .id("bookmark-bar")
            .debug_selector(|| "bookmark-bar".into())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .px(px(8.0))
            .pb(px(4.0))
            .overflow_x_scroll()
            .children(chips)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 칩_툴팁은_제목과_url을_두_줄로_보여준다() {
        let bookmark = Bookmark {
            id: "b1".into(),
            url: "https://example.com/docs".into(),
            title: "Docs".into(),
            added_at: 1,
        };
        assert_eq!(chip_tooltip(&bookmark), "Docs\nhttps://example.com/docs");
    }

    #[test]
    fn 제목이_130px를_넘을_때만_너비를_고정한다() {
        assert_eq!(title_width(40.0), None);
        assert_eq!(title_width(130.0), None);
        assert_eq!(title_width(130.5), Some(130.0));
        assert_eq!(title_width(900.0), Some(130.0));
    }

    #[test]
    fn 칩에_들어가면_그_칩이_호버_대상이_된다() {
        assert_eq!(hovered_after(None, "b1", true), Some("b1".to_string()));
        assert_eq!(
            hovered_after(Some("b1"), "b2", true),
            Some("b2".to_string())
        );
    }

    #[test]
    fn 호버_중인_칩에서_나가면_호버_대상이_없어진다() {
        assert_eq!(hovered_after(Some("b1"), "b1", false), None);
        assert_eq!(hovered_after(None, "b1", false), None);
    }

    #[test]
    fn 다른_칩의_나감_알림은_지금_호버를_지우지_않는다() {
        // b1 → b2로 옮길 때 b2의 들어옴이 b1의 나감보다 먼저 올 수 있다.
        assert_eq!(
            hovered_after(Some("b2"), "b1", false),
            Some("b2".to_string())
        );
    }
}

/// The view in a window: clicks and the hover-only ×.
#[cfg(test)]
mod view_tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    fn open<'a>(
        urls: &[&str],
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<BookmarksStore>,
        Entity<BookmarkBar>,
        &'a mut VisualTestContext,
    ) {
        let mut store = BookmarksStore::new(Box::new(MemoryStorage::default()));
        for url in urls {
            store.add(url, "A bookmark");
        }
        let store = cx.new(|_| store);
        let (bar, cx) = cx.add_window_view({
            let store = store.clone();
            move |_, cx| BookmarkBar::new(store, Lang::En, ThemeMode::Light, cx)
        });
        (store, bar, cx)
    }

    fn center(selector: &'static str, cx: &mut VisualTestContext) -> gpui::Point<gpui::Pixels> {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"))
            .center()
    }

    fn urls(store: &Entity<BookmarksStore>, cx: &mut VisualTestContext) -> Vec<String> {
        store.read_with(cx, |store, _| {
            store.bookmarks().iter().map(|b| b.url.clone()).collect()
        })
    }

    #[gpui::test]
    fn 북마크가_없으면_아무것도_그리지_않는다(cx: &mut TestAppContext) {
        let (store, _, cx) = open(&[], cx);
        assert!(cx.debug_bounds("bookmark-bar").is_none());
        // 북마크가 생기면 막대가 나타난다.
        store.update(cx, |store, cx| {
            store.add("https://a.test/", "A");
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("bookmark-bar").is_some());
    }

    #[gpui::test]
    fn 칩을_누르면_그_url을_열어_달라고_알린다(cx: &mut TestAppContext) {
        let (_, bar, cx) = open(&["https://a.test/", "https://b.test/"], cx);
        let mut opened = cx.events::<OpenUrl, _>(&bar);
        let second = center("bookmark-chip-1", cx);
        cx.simulate_click(second, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            opened.try_recv().ok(),
            Some(OpenUrl("https://b.test/".into()))
        );
    }

    #[gpui::test]
    fn 칩은_높이_24에_간격_2로_놓인다(cx: &mut TestAppContext) {
        let (_, _, cx) = open(&["https://a.test/", "https://b.test/"], cx);
        let first = cx.debug_bounds("bookmark-chip-0").unwrap();
        let second = cx.debug_bounds("bookmark-chip-1").unwrap();
        assert_eq!(first.size.height, px(24.0));
        assert_eq!(first.left(), px(8.0));
        assert_eq!(second.left() - first.right(), px(2.0));
    }

    #[gpui::test]
    fn x는_포인터가_칩_위에_있을_때만_보인다(cx: &mut TestAppContext) {
        let (store, bar, cx) = open(&["https://a.test/", "https://b.test/"], cx);
        let ids: Vec<String> = store.read_with(cx, |store, _| {
            store.bookmarks().iter().map(|b| b.id.clone()).collect()
        });
        // GPUI keeps the bounds of elements that are gone, so which chip shows the × is
        // read from the view; the bounds only prove that it was drawn.
        let hovered = |cx: &mut VisualTestContext| bar.read_with(cx, |bar, _| bar.hovered.clone());
        assert!(cx.debug_bounds("bookmark-chip-x-0").is_none());
        assert_eq!(hovered(cx), None);

        let first = center("bookmark-chip-0", cx);
        cx.simulate_mouse_move(first, None, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(hovered(cx).as_ref(), ids.first());
        let x = cx.debug_bounds("bookmark-chip-x-0").unwrap();
        assert_eq!(x.size, gpui::size(px(14.0), px(14.0)));
        assert!(cx.debug_bounds("bookmark-chip-x-1").is_none());

        // 다른 칩으로 옮기면 ×도 옮겨 간다.
        let second = center("bookmark-chip-1", cx);
        cx.simulate_mouse_move(second, None, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(hovered(cx).as_ref(), ids.get(1));
        assert!(cx.debug_bounds("bookmark-chip-x-1").is_some());

        // 막대 밖으로 나가면 사라진다.
        cx.simulate_mouse_move(gpui::point(px(400.0), px(300.0)), None, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(hovered(cx), None);
    }

    #[gpui::test]
    fn x를_누르면_북마크만_지우고_열지는_않는다(cx: &mut TestAppContext) {
        let (store, bar, cx) = open(&["https://a.test/", "https://b.test/"], cx);
        let mut opened = cx.events::<OpenUrl, _>(&bar);
        let first = center("bookmark-chip-0", cx);
        cx.simulate_mouse_move(first, None, Modifiers::none());
        cx.run_until_parked();
        let x = center("bookmark-chip-x-0", cx);
        cx.simulate_mouse_move(x, None, Modifiers::none());
        cx.simulate_click(x, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(urls(&store, cx), ["https://b.test/"]);
        assert!(opened.try_recv().is_err(), "the chip must not open");
    }

    #[gpui::test]
    fn x로_지운_칩의_자리에_온_칩도_포인터를_올리면_x를_보여준다(
        cx: &mut TestAppContext,
    ) {
        let (store, bar, cx) = open(&["https://a.test/", "https://b.test/"], cx);
        let second_id = store.read_with(cx, |store, _| store.bookmarks()[1].id.clone());
        let first = center("bookmark-chip-0", cx);
        cx.simulate_mouse_move(first, None, Modifiers::none());
        cx.run_until_parked();
        let x = center("bookmark-chip-x-0", cx);
        cx.simulate_mouse_move(x, None, Modifiers::none());
        cx.simulate_click(x, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(urls(&store, cx), ["https://b.test/"]);

        // b.test 칩이 첫 자리로 당겨져 왔다. 그 위로 포인터를 옮기면 그 칩이 호버 대상이다.
        cx.simulate_mouse_move(first, None, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            bar.read_with(cx, |bar, _| bar.hovered.clone()),
            Some(second_id)
        );
    }

    #[gpui::test]
    fn 긴_제목은_130px에서_잘린다(cx: &mut TestAppContext) {
        let mut store = BookmarksStore::new(Box::new(MemoryStorage::default()));
        store.add("https://a.test/", &"long title ".repeat(20));
        store.add("https://b.test/", "B");
        let store = cx.new(|_| store);
        let (_, cx) = cx
            .add_window_view(move |_, cx| BookmarkBar::new(store, Lang::En, ThemeMode::Light, cx));
        // 좌우 8, 파비콘 16, 간격 6, 제목 130.
        let chip = cx.debug_bounds("bookmark-chip-0").unwrap();
        assert_eq!(chip.size.width, px(8.0 + 16.0 + 6.0 + 130.0 + 8.0));
    }
}
