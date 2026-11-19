//! Opens this crate's views alone with fixture data. See rever_ui_kit::preview.
//!
//! The frame strip holds a tab bar 800px wide with nine tabs, so some of them move into
//! the "⋯ N" menu: a plain active tab, an isolated proxy tab (dot), a profile tab
//! (label) and one with a long title. Under it a page pane with the find bar open, a
//! bottom panel resized by the strip on its top edge, and a side pane resized by the
//! bar splitter. The line in the page names the last thing a widget asked the host for.
//!
//! `REVER_PREVIEW=menu` starts with the "⋯ N" menu open. `REVER_PREVIEW=markup` puts
//! the markup editor over all of it, on a capture that already has one shape of each
//! tool drawn on it.

use std::cell::RefCell;
use std::rc::Rc;

use base64::Engine as _;
use gpui::{App, Context, Entity, Window, div, prelude::*, px};
use image::{Rgba, RgbaImage};
use rever_ui_chrome::find_in_page::{FindBar, FindHost, FindOptions, FindResult};
use rever_ui_chrome::markup_editor::{
    CanvasPoint, MarkupEditor, MarkupEvent, Shape, draw_shape, encode_png, line_width,
};
use rever_ui_chrome::resizable::{Axis, HandleLook, Resizable, ResizeOptions, Side};
use rever_ui_chrome::tab_bar::{TabBar, TabBarEvent, TabBarProps, TabProfile};
use rever_ui_chrome::ui::{Look, LookFn};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use rever_ui_state::MemoryStorage;
use rever_ui_state::tabs::{ProxyConfig, ProxyScheme, Tab};

const WIDTH: f32 = 1000.0;
const HEIGHT: f32 = 680.0;
const TAB_BAR_WIDTH: f32 = 800.0;

type Say = Rc<dyn Fn(String, &mut App)>;

/// Stands in for the tab handles: every page can be searched, and says so.
struct FakeFind(Say);

impl FindHost for FakeFind {
    fn has_page(&self, _: &str, _: &App) -> bool {
        true
    }

    fn find_in_page(&self, tab_id: &str, query: &str, options: Option<FindOptions>, cx: &mut App) {
        (self.0)(format!("findInPage({tab_id}, {query:?}, {options:?})"), cx);
    }

    fn stop_find_in_page(&self, tab_id: &str, cx: &mut App) {
        (self.0)(format!("stopFindInPage({tab_id})"), cx);
    }
}

fn tab(id: &str, title: &str) -> Tab {
    let url = format!("https://{id}.example.com/");
    Tab {
        id: id.into(),
        url: url.clone(),
        title: title.into(),
        web_contents_id: None,
        initial_url: url,
        proxy: None,
        partition: None,
        profile_id: None,
    }
}

fn tabs() -> TabBarProps {
    let mut tabs = vec![
        tab("t1", "GitHub"),
        tab("t2", "Proxy check"),
        tab("t3", "Work mail"),
        tab(
            "t4",
            "A very long page title that does not fit the widest tab",
        ),
        tab("t5", "Docs"),
        tab("t6", "Hacker News"),
        tab("t7", ""),
        tab(
            "t8",
            "Search results for a query so long that the menu cuts it too",
        ),
        tab("t9", "Last tab"),
    ];
    tabs[1].partition = Some("proxy-t2".into());
    tabs[1].proxy = Some(ProxyConfig {
        enabled: true,
        scheme: ProxyScheme::Socks5,
        host: "127.0.0.1".into(),
        port: 9050,
        username: None,
        password: None,
    });
    tabs[2].partition = Some("persist:profile-p1".into());
    tabs[2].profile_id = Some("p1".into());
    TabBarProps {
        tabs,
        active_id: "t1".into(),
        profiles: vec![TabProfile {
            id: "p1".into(),
            name: "Work".into(),
        }],
    }
}

/// A capture to mark up: stripes of pure red, green and blue on white (so a swapped
/// channel shows), with a rectangle, an arrow and a pen stroke already drawn on it.
fn capture() -> String {
    let (width, height) = (960, 540);
    let mut image = RgbaImage::from_fn(width, height, |x, y| {
        let stripe = match x / 80 {
            1 => [255, 0, 0, 255],
            2 => [0, 255, 0, 255],
            3 => [0, 0, 255, 255],
            _ => [255, 255, 255, 255],
        };
        Rgba(if y < 60 { stripe } else { [246, 247, 249, 255] })
    });
    let at = |x: f32, y: f32| CanvasPoint { x, y };
    let lw = line_width(width);
    let shapes = [
        Shape::Rect {
            color: "#ef4444",
            x: 120.0,
            y: 130.0,
            w: 300.0,
            h: 160.0,
        },
        Shape::Arrow {
            color: "#3b82f6",
            x1: 760.0,
            y1: 420.0,
            x2: 470.0,
            y2: 230.0,
        },
        Shape::Pen {
            color: "#22c55e",
            points: (0..=60)
                .map(|step| {
                    let x = 520.0 + step as f32 * 6.0;
                    at(x, 400.0 + (step as f32 / 5.0).sin() * 40.0)
                })
                .collect(),
        },
        Shape::Pen {
            color: "#111111",
            points: vec![at(150.0, 400.0), at(260.0, 470.0), at(380.0, 380.0)],
        },
    ];
    for shape in &shapes {
        draw_shape(&mut image, shape, lw);
    }
    let png = encode_png(&image).unwrap_or_default();
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    )
}

struct Preview {
    look: Look,
    last: Rc<RefCell<String>>,
    tab_bar: Entity<TabBar>,
    find: Entity<FindBar>,
    side: Entity<Resizable>,
    bottom: Entity<Resizable>,
    markup: Option<Entity<MarkupEditor>>,
}

impl Preview {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let look = Look {
            theme_mode: ThemeMode::from_env(),
            lang: Lang::detect(),
        };
        let look_fn: LookFn = Rc::new(move |_| look);
        let last = Rc::new(RefCell::new(String::from("(nothing yet)")));
        let say: Say = {
            let (last, preview) = (last.clone(), cx.entity_id());
            Rc::new(move |text: String, cx: &mut App| {
                *last.borrow_mut() = text;
                cx.notify(preview);
            })
        };

        let props = Rc::new(RefCell::new(tabs()));
        let tab_bar = cx.new({
            let props = props.clone();
            |_| TabBar::new(move |_| props.borrow().clone(), look_fn.clone())
        });
        let mode = std::env::var("REVER_PREVIEW").unwrap_or_default();
        if mode == "menu" {
            tab_bar.update(cx, |bar, cx| bar.toggle_menu(cx));
        }
        let find = cx.new(|cx| FindBar::new(Rc::new(FakeFind(say.clone())), look_fn.clone(), cx));
        find.update(cx, |find, cx| {
            find.set_active_tab(Some("t1".into()), cx);
            find.open(window, cx);
            find.set_query("rever", cx);
            find.found_in_page(
                "t1",
                FindResult {
                    active: 2,
                    total: 5,
                },
                cx,
            );
        });
        // What the store would do with the bar's events, so the preview can be clicked.
        cx.subscribe(&tab_bar, {
            let (say, find) = (say.clone(), find.clone());
            move |_, _, event: &TabBarEvent, cx| {
                let mut props = props.borrow_mut();
                match event {
                    TabBarEvent::Select(id) => props.active_id.clone_from(id),
                    TabBarEvent::Close(id) => {
                        props.tabs.retain(|tab| &tab.id != id);
                        if &props.active_id == id {
                            props.active_id = props
                                .tabs
                                .last()
                                .map(|tab| tab.id.clone())
                                .unwrap_or_default();
                        }
                    }
                    TabBarEvent::NewTab(url) => {
                        let id = format!("n{}", props.tabs.len());
                        props.tabs.push(Tab {
                            url: url.clone(),
                            ..tab(&id, "New Tab")
                        });
                        props.active_id = id;
                    }
                }
                let active = Some(props.active_id.clone());
                drop(props);
                find.update(cx, |find, cx| find.set_active_tab(active, cx));
                say(format!("{event:?}"), cx);
            }
        })
        .detach();

        let storage = Rc::new(MemoryStorage::new());
        let resizable = |options: ResizeOptions, handle: HandleLook, cx: &mut Context<Self>| {
            let (storage, look) = (storage.clone(), look_fn.clone());
            cx.new(|_| Resizable::new(options, handle, storage, look))
        };
        // The chat panel's splitter and the bottom panel's, as App.tsx and
        // FloatingChips.tsx configure them.
        let side = resizable(
            ResizeOptions {
                initial: 300.0,
                min: 200.0,
                max: 480.0,
                storage_key: Some("rev:chat-w".into()),
                side: Side::Left,
                axis: Axis::X,
            },
            HandleLook::Bar,
            cx,
        );
        let bottom = resizable(
            ResizeOptions {
                initial: 180.0,
                min: 120.0,
                max: 400.0,
                storage_key: Some("rev:bottom-h".into()),
                side: Side::Top,
                axis: Axis::Y,
            },
            HandleLook::Edge,
            cx,
        );

        let markup = (mode == "markup").then(|| {
            let editor = cx.new(|cx| MarkupEditor::new(&capture(), look_fn.clone(), cx));
            cx.subscribe(&editor, move |this, _, event: &MarkupEvent, cx| {
                let text = match event {
                    MarkupEvent::Close => {
                        this.markup = None;
                        "onClose()".to_owned()
                    }
                    MarkupEvent::Save { file_name, png } => {
                        format!("save {file_name} ({} bytes)", png.len())
                    }
                };
                say(text, cx);
            })
            .detach();
            editor
        });

        Self {
            look,
            last,
            tab_bar,
            find,
            side,
            bottom,
            markup,
        }
    }
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.look.theme_mode, window.appearance());
        let side_width = self.side.read(cx).width();
        let bottom_height = self.bottom.read(cx).width();

        let frame = div()
            .h(px(40.0))
            .flex()
            .flex_none()
            .px(px(8.0))
            .bg(theme.chrome_frame)
            .child(
                div()
                    .w(px(TAB_BAR_WIDTH))
                    .h_full()
                    .flex()
                    .child(self.tab_bar.clone()),
            );
        let toolbar = div()
            .h(px(46.0))
            .flex_none()
            .px(px(16.0))
            .flex()
            .items_center()
            .bg(theme.chrome_toolbar)
            .text_color(theme.text_dim)
            .child(format!("last: {}", self.last.borrow()));

        let page = div()
            .flex_1()
            .min_h_0()
            .relative()
            .p(px(16.0))
            .bg(theme.viewport_bg)
            .child("Page")
            .child(self.find.clone());
        let bottom = div()
            .h(px(bottom_height))
            .flex_none()
            .relative()
            .p(px(12.0))
            .bg(theme.bg_bar)
            .border_t_1()
            .border_color(theme.border)
            .child(format!(
                "Bottom panel — {bottom_height}px, drag its top edge"
            ))
            .child(self.bottom.clone());
        let left = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(page)
            .child(bottom);
        let side = div()
            .w(px(side_width))
            .flex_none()
            .p(px(12.0))
            .bg(theme.bg)
            .child(format!("Side panel — {side_width}px"));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.0))
            .font_family(".SystemUIFont")
            .child(frame)
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(left)
                    .child(self.side.clone())
                    .child(side),
            )
            .children(self.markup.clone())
    }
}

fn main() {
    rever_ui_kit::preview::run(WIDTH, HEIGHT, |window, cx| {
        cx.new(|cx| Preview::new(window, cx))
    });
}
