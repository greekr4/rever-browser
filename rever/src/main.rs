//! rever: a GPUI interface around Servo page views.
//!
//! Usage: rever [--panel=NAME] [--self-check=DIR] [URL]
//!   --panel=NAME      start with a bottom panel open (traffic, console, ...).
//!   --self-check=DIR  after 8 seconds write DIR/frame.png (the page as handed to the
//!                     interface) and DIR/state.json, then quit. Used by the port harness.
//!
//! Layout mirrors the Electron app's `src/renderer/src/App.tsx`: tab strip, toolbar,
//! page, bottom panel, chip bar on the left; the chat panel on the right.

mod engine;
mod i18n;
mod icons;
mod locales;
mod text_input;
mod theme;

use std::cell::Cell;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dpi::PhysicalSize;
use engine::{Engine, EngineEvent, TabId};
use euclid::Point2D;
use futures::StreamExt;
use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, Context, Corners, Div, Entity, FocusHandle,
    Focusable, FontWeight, KeyDownEvent, KeyUpEvent, Keystroke, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderImage, Rgba, ScrollDelta, ScrollWheelEvent,
    SharedString, Stateful, Window, WindowBounds, WindowOptions, canvas, div, prelude::*, px, size,
    svg,
};
use i18n::Lang;
use image::{Frame, RgbaImage};
use net::tap::NetworkTapEvent;
use rever_core::address::normalize_address;
use rever_core::input::w3c_key_name;
use rever_core::traffic::{self, Headers, ListFilter, RequestPatch, TrafficStore};
use servo::{
    Code, InputEvent, Key, KeyState, KeyboardEvent, Location, Modifiers, MouseButtonAction,
    MouseButtonEvent, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
};
use text_input::{Submitted, TextInput};
use theme::{Theme, ThemeMode};
use url::Url;

const DEFAULT_URL: &str = "https://github.com/greekr4/rever-browser";
/// What the "+" button opens, as in TabBar.tsx.
const NEW_TAB_URL: &str = "https://www.google.com";
const CHAT_WIDTH: f32 = 420.0;
const PANEL_HEIGHT: f32 = 320.0;
const MAX_CONSOLE: usize = 1000;
const CHAT_EMPTY: &str = "Open a site in the browser and ask me to analyze its API traffic, or describe an action you just performed and I'll dig into the requests it triggered.";

/// The bottom panels, grouped as in FloatingChips.tsx: related panels sit together.
const PANEL_GROUPS: &[&[&str]] = &[
    &["traffic", "apimap", "websocket"],
    &["console", "exceptions"],
    &["repeater", "wsrepeater", "storage"],
    &["history", "workflows"],
];

fn panel_title_key(panel: &str) -> &'static str {
    match panel {
        "traffic" => "panel.traffic",
        "apimap" => "panel.apimap",
        "websocket" => "panel.websocket",
        "console" => "panel.console",
        "exceptions" => "panel.exceptions",
        "repeater" => "panel.repeater",
        "wsrepeater" => "panel.wsrepeater",
        "storage" => "panel.storage",
        "history" => "panel.history",
        _ => "panel.workflows",
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Runs on Servo's network threads: record what the tap saw.
fn record(store: &Mutex<TrafficStore>, event: NetworkTapEvent) {
    fn headers<'a>(pairs: impl Iterator<Item = (&'a str, &'a [u8])>) -> Headers {
        let mut map = Headers::new();
        for (name, value) in pairs {
            let value = String::from_utf8_lossy(value).into_owned();
            // Repeated headers are joined with a newline, as CDP does.
            map.entry(name.to_owned())
                .and_modify(|all| *all = format!("{all}\n{value}"))
                .or_insert(value);
        }
        map
    }
    let patch = match event {
        NetworkTapEvent::Request {
            id,
            url,
            method,
            headers: h,
            body,
            destination,
        } => RequestPatch {
            request_id: id,
            url: Some(url.to_string()),
            method: Some(method.to_string()),
            resource_type: Some(traffic::resource_type_of(&destination).to_owned()),
            request_headers: Some(headers(h.iter().map(|(n, v)| (n.as_str(), v.as_bytes())))),
            request_post_data: (!body.is_empty())
                .then(|| String::from_utf8_lossy(&body).into_owned()),
            ..RequestPatch::default()
        },
        NetworkTapEvent::Response {
            id,
            url,
            status,
            headers: h,
            body,
        } => {
            let response_headers = headers(h.iter().map(|(n, v)| (n.as_str(), v.as_bytes())));
            let mime = traffic::mime_of(&response_headers);
            let (stored, base64) = traffic::encode_body(&body, &mime);
            RequestPatch {
                request_id: id,
                url: Some(url.to_string()),
                status: Some(status),
                completed_at: Some(now_ms()),
                encoded_data_length: Some(body.len() as u64),
                mime_type: Some(mime),
                response_headers: Some(response_headers),
                response_body: Some(stored),
                response_body_base64: Some(base64),
                ..RequestPatch::default()
            }
        }
    };
    store.lock().unwrap().upsert(patch, now_ms());
}

struct Tab {
    id: TabId,
    title: SharedString,
    url: String,
    /// The latest page frame, in the BGRA layout GPUI paints.
    frame: Option<Arc<RenderImage>>,
}

struct ChatLine {
    from_user: bool,
    text: SharedString,
}

struct Browser {
    engine: Engine,
    tabs: Vec<Tab>,
    active: usize,
    address: Entity<TextInput>,
    composer: Entity<TextInput>,
    chat: Vec<ChatLine>,
    open_panel: Option<&'static str>,
    console: Vec<(String, String)>,
    /// Where the page was laid out last, in window coordinates. Written during paint.
    page_bounds: Rc<Cell<Bounds<Pixels>>>,
    traffic: Arc<Mutex<TrafficStore>>,
    page_focus: FocusHandle,
    lang: Lang,
    theme_mode: ThemeMode,
    /// The active tab's frame as RGBA, kept only for --self-check.
    last_rgba: Option<RgbaImage>,
    self_check: Option<String>,
}

impl Browser {
    fn new(
        url: Url,
        open_panel: Option<&'static str>,
        self_check: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lang = Lang::detect();
        let (wake, mut woken) = futures::channel::mpsc::unbounded::<()>();
        let traffic = Arc::new(Mutex::new(TrafficStore::default()));
        {
            let (store, wake) = (traffic.clone(), wake.clone());
            net::tap::set_network_tap(move |event| {
                record(&store, event);
                let _ = wake.unbounded_send(());
            });
        }
        let mut engine = Engine::new(
            move || {
                let _ = wake.unbounded_send(());
            },
            window.scale_factor(),
        );
        let first = engine
            .open(url.clone(), PhysicalSize::new(800, 600))
            .expect("the page engine could not start");

        // Servo and the tap wake this task from their threads; the work happens here, on the main thread.
        cx.spawn_in(window, async move |this, cx| {
            while woken.next().await.is_some() {
                while woken.try_recv().is_ok() {}
                if this
                    .update_in(cx, |this, window, cx| this.pump(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        if let Some(dir) = self_check.clone() {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(8)).await;
                let _ = this.update(cx, |this, cx| {
                    this.write_self_check(&dir);
                    cx.quit();
                });
            })
            .detach();
        }

        let address = cx.new(|cx| {
            let mut input = TextInput::new(lang.t("toolbar.addressPlaceholder"), cx);
            input.set_text(url.to_string(), cx);
            input
        });
        cx.subscribe(&address, |this, _, event: &Submitted, cx| {
            this.navigate(&event.0, cx)
        })
        .detach();
        let composer = cx.new(|cx| TextInput::new(lang.t("chat.placeholder"), cx));
        cx.subscribe(&composer, |this, _, event: &Submitted, cx| {
            this.send_chat(&event.0, cx)
        })
        .detach();

        Self {
            engine,
            tabs: vec![Tab {
                id: first,
                title: "".into(),
                url: url.to_string(),
                frame: None,
            }],
            active: 0,
            address,
            composer,
            chat: Vec::new(),
            open_panel,
            console: Vec::new(),
            page_bounds: Rc::new(Cell::new(Bounds::default())),
            traffic,
            page_focus: cx.focus_handle(),
            lang,
            theme_mode: ThemeMode::from_env(),
            last_rgba: None,
            self_check,
        }
    }

    fn active_id(&self) -> TabId {
        self.tabs[self.active].id
    }

    fn pump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active_id();
        let mut frame_ready = false;
        for event in self.engine.pump() {
            match event {
                EngineEvent::FrameReady(tab) => frame_ready |= tab == active,
                EngineEvent::Title(tab, title) => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab) {
                        tab.title = title.into();
                    }
                }
                EngineEvent::Url(tab, url) => {
                    // Do not overwrite what the user is typing.
                    if tab == active && !self.address.focus_handle(cx).is_focused(window) {
                        self.address
                            .update(cx, |input, cx| input.set_text(url.clone(), cx));
                    }
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab) {
                        tab.url = url;
                    }
                }
                EngineEvent::Console { level, message } => {
                    self.console.push((level, message));
                    if self.console.len() > MAX_CONSOLE {
                        self.console.remove(0);
                    }
                }
            }
        }
        if frame_ready {
            self.capture(window);
        }
        cx.notify();
    }

    /// Paint the active tab and keep the result for the next render.
    fn capture(&mut self, window: &mut Window) {
        let Some(mut image) = self.engine.frame(self.active_id()) else {
            return;
        };
        if self.self_check.is_some() {
            self.last_rgba = Some(image.clone());
        }
        // GPUI paints BGRA.
        for pixel in image.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let frame = Arc::new(RenderImage::new(vec![Frame::new(image)]));
        if let Some(old) = self.tabs[self.active].frame.replace(frame) {
            let _ = window.drop_image(old);
        }
    }

    /// Make the active page as large as the area the interface gave it.
    fn fit_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = self.page_bounds.get();
        let scale = window.scale_factor();
        let device = |length: Pixels| (f32::from(length) * scale).round().max(1.0) as u32;
        let size = PhysicalSize::new(device(bounds.size.width), device(bounds.size.height));
        if self.engine.size(self.active_id()) != Some(size) {
            self.engine.resize(self.active_id(), size);
            self.pump(window, cx);
        }
    }

    fn navigate(&mut self, input: &str, cx: &mut Context<Self>) {
        let Ok(url) = Url::parse(&normalize_address(input)) else {
            return;
        };
        self.engine.load(self.active_id(), url);
        cx.notify();
    }

    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(url) = Url::parse(NEW_TAB_URL) else {
            return;
        };
        let size = self
            .engine
            .size(self.active_id())
            .unwrap_or(PhysicalSize::new(800, 600));
        let Ok(id) = self.engine.open(url, size) else {
            return;
        };
        self.tabs.push(Tab {
            id,
            title: "".into(),
            url: NEW_TAB_URL.into(),
            frame: None,
        });
        self.select_tab(self.tabs.len() - 1, window, cx);
    }

    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        let url = self.tabs[index].url.clone();
        self.address.update(cx, |input, cx| input.set_text(url, cx));
        self.fit_page(window, cx);
        self.capture(window);
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        // Never close the last tab, as in the tabs store.
        if self.tabs.len() == 1 || index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        self.engine.close(tab.id);
        if let Some(frame) = tab.frame {
            let _ = window.drop_image(frame);
        }
        let next = if index < self.active {
            self.active - 1
        } else {
            self.active.min(self.tabs.len() - 1)
        };
        self.select_tab(next, window, cx);
    }

    fn toggle_panel(&mut self, panel: &'static str, cx: &mut Context<Self>) {
        self.open_panel = if self.open_panel == Some(panel) {
            None
        } else {
            Some(panel)
        };
        cx.notify();
    }

    fn send_chat(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.chat.push(ChatLine {
            from_user: true,
            text: text.to_owned().into(),
        });
        self.chat.push(ChatLine {
            from_user: false,
            text: self.lang.t("port.agentNotYet").into(),
        });
        self.composer.update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    fn page_point(&self, position: Point<Pixels>) -> WebViewPoint {
        let origin = self.page_bounds.get().origin;
        WebViewPoint::Page(Point2D::new(
            f32::from(position.x - origin.x),
            f32::from(position.y - origin.y),
        ))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.focus(&self.page_focus);
        let point = self.page_point(event.position);
        let tab = self.active_id();
        self.engine.input(
            tab,
            InputEvent::MouseMove(servo::MouseMoveEvent::new(point)),
        );
        self.engine.input(
            tab,
            InputEvent::MouseButton(MouseButtonEvent::new(
                MouseButtonAction::Down,
                servo::MouseButton::Primary,
                point,
            )),
        );
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.engine.input(
            self.active_id(),
            InputEvent::MouseButton(MouseButtonEvent::new(
                MouseButtonAction::Up,
                servo::MouseButton::Primary,
                self.page_point(event.position),
            )),
        );
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        let point = self.page_point(event.position);
        self.engine.input(
            self.active_id(),
            InputEvent::MouseMove(servo::MouseMoveEvent::new(point)),
        );
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, _: &mut Context<Self>) {
        // Both sides use "positive scrolls up/left"; Servo's pixel deltas are device pixels.
        let scale = f64::from(window.scale_factor());
        let (x, y, mode) = match event.delta {
            ScrollDelta::Pixels(delta) => (
                f64::from(f32::from(delta.x)) * scale,
                f64::from(f32::from(delta.y)) * scale,
                WheelMode::DeltaPixel,
            ),
            ScrollDelta::Lines(delta) => {
                (f64::from(delta.x), f64::from(delta.y), WheelMode::DeltaLine)
            }
        };
        self.engine.input(
            self.active_id(),
            InputEvent::Wheel(WheelEvent::new(
                WheelDelta { x, y, z: 0.0, mode },
                self.page_point(event.position),
            )),
        );
    }

    fn on_key(&mut self, state: KeyState, keystroke: &Keystroke) {
        let shortcut = keystroke.modifiers.platform || keystroke.modifiers.control;
        let key = match (w3c_key_name(&keystroke.key), keystroke.key_char.as_deref()) {
            (Some(name), _) => Key::from_str(name).ok(),
            (None, Some(text)) if !shortcut => Some(Key::Character(text.to_owned())),
            (None, _) if keystroke.key == "space" => Some(Key::Character(" ".into())),
            (None, _) => Some(Key::Character(keystroke.key.clone())),
        };
        let Some(key) = key else { return };
        let mut modifiers = Modifiers::empty();
        modifiers.set(Modifiers::SHIFT, keystroke.modifiers.shift);
        modifiers.set(Modifiers::CONTROL, keystroke.modifiers.control);
        modifiers.set(Modifiers::ALT, keystroke.modifiers.alt);
        modifiers.set(Modifiers::META, keystroke.modifiers.platform);
        self.engine.input(
            self.active_id(),
            InputEvent::Keyboard(KeyboardEvent::new_without_event(
                state,
                key,
                Code::Unidentified,
                Location::Standard,
                modifiers,
                false,
                false,
            )),
        );
    }

    fn write_self_check(&self, dir: &str) {
        let _ = std::fs::create_dir_all(dir);
        let frame = self.last_rgba.as_ref();
        if let Some(frame) = frame {
            let _ = frame.save(format!("{dir}/frame.png"));
        }
        let store = self.traffic.lock().unwrap();
        let requests: Vec<_> = store.list(&ListFilter {
            limit: Some(500),
            ..Default::default()
        });
        let tab = &self.tabs[self.active];
        let state = serde_json::json!({
            "title": tab.title.to_string(),
            "url": tab.url,
            "tabs": self.tabs.len(),
            "frame": frame.map(|f| [f.width(), f.height()]),
            // A blank frame has one color; a rendered page has several.
            "frameColors": frame.map(|f| {
                f.pixels().step_by(16).map(|p| p.0).collect::<std::collections::HashSet<_>>().len()
            }),
            "requests": requests,
        });
        let _ = std::fs::write(format!("{dir}/state.json"), state.to_string());
    }
}

// ── Elements ────────────────────────────────────────────────────────────────

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

impl Browser {
    fn tab_strip(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = self.lang;
        let closable = self.tabs.len() > 1;
        div()
            .h(px(40.0))
            .flex()
            .flex_none()
            .items_end()
            .gap(px(2.0))
            .px(px(8.0))
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
    }

    fn toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = self.lang;
        div()
            .h(px(46.0))
            .px(px(8.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .bg(theme.chrome_toolbar)
            .border_b_1()
            .border_color(theme.border)
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
                // `.omnibox`
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(34.0))
                    .mx(px(4.0))
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .rounded(px(17.0))
                    .bg(theme.chrome_omnibox)
                    .text_size(px(14.0))
                    .text_color(theme.text)
                    .child(self.address.clone()),
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
            .child(
                div()
                    .px(px(8.0))
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(theme.text_dim)
                    .child(lang.t("ip.none")),
            )
            .child(icon_button("profile", "profile", 32.0, theme))
            .child(icon_button("menu", "more", 32.0, theme))
    }

    fn page(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.tabs[self.active].frame.clone();
        let bounds_cell = self.page_bounds.clone();
        let this = cx.entity();
        let painted = canvas(
            move |bounds, window, _| {
                // The engine is resized after this frame: entities cannot be updated mid-paint.
                if bounds_cell.get() != bounds {
                    bounds_cell.set(bounds);
                    window.on_next_frame(move |window, cx| {
                        this.update(cx, |this, cx| this.fit_page(window, cx));
                    });
                }
            },
            move |bounds, _, window, _| {
                if let Some(frame) = frame {
                    let _ = window.paint_image(bounds, Corners::default(), frame, 0, false);
                }
            },
        )
        .size_full();

        div()
            .id("page")
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(theme.viewport_bg)
            .track_focus(&self.page_focus)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, _| {
                this.on_key(KeyState::Down, &event.keystroke)
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| {
                this.on_key(KeyState::Up, &event.keystroke)
            }))
            .child(painted)
    }

    fn panel(&self, panel: &'static str, theme: &Theme) -> impl IntoElement {
        let body: AnyElement = match panel {
            "traffic" => {
                let store = self.traffic.lock().unwrap();
                let rows = store.list(&ListFilter {
                    limit: Some(200),
                    ..Default::default()
                });
                div()
                    .children(rows.iter().map(|request| {
                        let status = request
                            .status
                            .map_or("…".to_owned(), |status| status.to_string());
                        div()
                            .h(px(24.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .text_size(px(12.0))
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .w(px(46.0))
                                    .flex_none()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(request.method.clone()),
                            )
                            .child(
                                div()
                                    .w(px(34.0))
                                    .flex_none()
                                    .text_color(theme.http_status(request.status))
                                    .child(status),
                            )
                            .child(
                                div()
                                    .w(px(76.0))
                                    .flex_none()
                                    .text_color(theme.text_dim)
                                    .child(request.resource_type.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(request.url.clone()),
                            )
                    }))
                    .into_any_element()
            }
            "console" => div()
                .children(self.console.iter().rev().take(300).map(|(level, message)| {
                    let color = match level.as_str() {
                        "error" => theme.status_error,
                        "warn" => theme.status_warn,
                        _ => theme.text,
                    };
                    div()
                        .px(px(12.0))
                        .py(px(3.0))
                        .text_size(px(12.0))
                        .text_color(color)
                        .border_b_1()
                        .border_color(theme.border)
                        .child(message.clone())
                }))
                .into_any_element(),
            _ => div()
                .p(px(24.0))
                .text_color(theme.text_dim)
                .child(self.lang.t("port.panelNotYet"))
                .into_any_element(),
        };
        div()
            .h(px(PANEL_HEIGHT))
            .flex()
            .flex_col()
            .flex_none()
            .bg(theme.bg)
            .border_t_1()
            .border_color(theme.border_2)
            .child(
                div()
                    .h(px(30.0))
                    .px(px(12.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(self.lang.t(panel_title_key(panel))),
            )
            .child(
                div()
                    .id("panel-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
    }

    fn chip_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = self.lang;
        let counts = [
            ("traffic", self.traffic.lock().unwrap().len()),
            ("console", self.console.len()),
        ];
        let mut bar = div()
            .h(px(40.0))
            .px(px(8.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .overflow_hidden()
            .bg(theme.bg_bar)
            .border_t_1()
            .border_color(theme.border);
        for (group_index, group) in PANEL_GROUPS.iter().enumerate() {
            if group_index > 0 {
                bar = bar.child(
                    div()
                        .w(px(1.0))
                        .h(px(16.0))
                        .mx(px(6.0))
                        .flex_none()
                        .bg(theme.border_2),
                );
            }
            for &panel in *group {
                let open = self.open_panel == Some(panel);
                let color = if open { theme.text } else { theme.text_dim };
                let hover = theme.chrome_hover;
                let count = counts
                    .iter()
                    .find(|(name, _)| *name == panel)
                    .map(|(_, count)| *count)
                    .filter(|count| *count > 0);
                bar = bar.child(
                    // `.chrome-chip`
                    div()
                        .id(panel)
                        .h(px(28.0))
                        .px(px(10.0))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(5.0))
                        .rounded(px(14.0))
                        .text_size(px(12.0))
                        .text_color(color)
                        .cursor_pointer()
                        .when(open, |chip| chip.bg(theme.surface_3))
                        .hover(move |style| style.bg(hover))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_panel(panel, cx)
                        }))
                        .child(icon(panel, 14.0, color))
                        .child(lang.t(panel_title_key(panel)))
                        .when_some(count, |chip, count| {
                            chip.child(
                                div()
                                    .px(px(6.0))
                                    .rounded(px(8.0))
                                    .bg(theme.surface_3)
                                    .text_size(px(11.0))
                                    .child(count.to_string()),
                            )
                        }),
                );
            }
        }
        bar.child(div().flex_1()).child(
            div()
                .h(px(28.0))
                .px(px(10.0))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(theme.border_2)
                .text_size(px(12.0))
                .text_color(theme.status_error)
                .child(icon("redteam", 14.0, theme.status_error))
                .child(lang.t("panel.redteam")),
        )
    }

    fn chat_panel(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = self.lang;
        let has_text = !self.composer.read(cx).text().is_empty();
        let footer_chip = |name: &'static str, label: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_size(px(12.0))
                .text_color(theme.text_dim)
                .child(icon(name, 13.0, theme.text_dim))
                .child(label)
        };
        div()
            .w(px(CHAT_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .bg(theme.bg)
            .border_l_1()
            .border_color(theme.border_2)
            .child(
                // `.chat-tabs`
                div()
                    .h(px(40.0))
                    .px(px(8.0))
                    .flex()
                    .flex_none()
                    .items_end()
                    .gap(px(2.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .h(px(32.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .rounded_t(px(8.0))
                            .bg(theme.surface_3)
                            .text_size(px(12.0))
                            .child("Claude Code")
                            .child(icon("close", 10.0, theme.text_dim)),
                    )
                    .child(div().pb(px(2.0)).child(icon_button(
                        "chat-new-tab",
                        "plus",
                        28.0,
                        theme,
                    ))),
            )
            .child(
                div()
                    .h(px(36.0))
                    .px(px(12.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_end()
                    .gap(px(14.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .border_b_1()
                    .border_color(theme.border)
                    .child("ready")
                    .child(format!("{} (0)", lang.t("chat.history")))
                    .child(
                        div()
                            .px(px(8.0))
                            .py(px(3.0))
                            .rounded(px(6.0))
                            .bg(theme.surface_2)
                            .child(lang.t("chat.newShort")),
                    ),
            )
            .child(
                div()
                    .id("chat-log")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .text_size(px(14.0))
                    .when(self.chat.is_empty(), |log| {
                        log.child(div().text_color(theme.text_dim).child(CHAT_EMPTY))
                    })
                    .children(self.chat.iter().map(|line| {
                        if line.from_user {
                            // `.chat-user-bubble`
                            div().flex().justify_end().child(
                                div()
                                    .max_w(px(CHAT_WIDTH - 80.0))
                                    .px(px(12.0))
                                    .py(px(8.0))
                                    .rounded(px(14.0))
                                    .bg(theme.surface_2)
                                    .child(line.text.clone()),
                            )
                        } else {
                            div().text_color(theme.text_dim).child(line.text.clone())
                        }
                    })),
            )
            .child(
                div().p(px(12.0)).flex_none().child(
                    div()
                        .p(px(12.0))
                        .flex()
                        .flex_col()
                        .gap(px(12.0))
                        .rounded(px(16.0))
                        .border_1()
                        .border_color(theme.border_2)
                        .bg(theme.chrome_toolbar)
                        .child(div().text_size(px(14.0)).child(self.composer.clone()))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(14.0))
                                .child(footer_chip("shield", lang.t("chat.perm.auto")))
                                .child(footer_chip("wrench", "Recon"))
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(theme.text_dim)
                                        .child("Claude Code"),
                                )
                                .child(
                                    // `.composer-send`
                                    div()
                                        .size(px(32.0))
                                        .flex()
                                        .flex_none()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(if has_text {
                                            theme.accent
                                        } else {
                                            theme.border_2
                                        })
                                        .child(icon("send", 16.0, theme.chrome_toolbar)),
                                ),
                        ),
                ),
            )
    }
}

impl Render for Browser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.theme_mode, window.appearance());
        let left = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(self.tab_strip(&theme, cx))
            .child(self.toolbar(&theme, cx))
            .child(self.page(&theme, cx))
            .when_some(self.open_panel, |column, panel| {
                column.child(self.panel(panel, &theme))
            })
            .child(self.chip_bar(&theme, cx));
        div()
            .size_full()
            .flex()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.0))
            .font_family(".SystemUIFont")
            .child(left)
            .child(self.chat_panel(&theme, cx))
    }
}

fn main() {
    let mut url = DEFAULT_URL.to_owned();
    let (mut self_check, mut open_panel) = (None, None);
    for arg in std::env::args().skip(1) {
        if let Some(dir) = arg.strip_prefix("--self-check=") {
            self_check = Some(dir.to_owned());
        } else if let Some(name) = arg.strip_prefix("--panel=") {
            // Only known panels: the name is also the chip's element id.
            open_panel = PANEL_GROUPS
                .iter()
                .flat_map(|group| group.iter())
                .copied()
                .find(|panel| *panel == name);
        } else {
            url = arg;
        }
    }
    let url = Url::parse(&normalize_address(&url)).expect("the URL argument is not a valid URL");

    Application::new()
        .with_assets(icons::Assets)
        .run(move |cx: &mut App| {
            text_input::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Browser::new(url, open_panel, self_check, window, cx)),
            )
            .expect("could not open the main window");
            cx.activate(true);
        });
}
