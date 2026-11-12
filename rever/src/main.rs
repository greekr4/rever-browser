//! rever: a GPUI interface around a Servo page view.
//!
//! Usage: rever [--self-check=DIR] [URL]
//!   --self-check=DIR  after 8 seconds write DIR/frame.png (the page as handed to the
//!                     interface) and DIR/state.json, then quit. Used by the port harness.

mod engine;

use std::cell::Cell;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dpi::PhysicalSize;
use engine::{Engine, EngineEvent};
use euclid::Point2D;
use futures::StreamExt;
use futures::channel::mpsc::UnboundedSender;
use gpui::{
    App, Application, Bounds, Context, Corners, FocusHandle, KeyDownEvent, KeyUpEvent, Keystroke,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderImage,
    ScrollDelta, ScrollWheelEvent, SharedString, Window, WindowBounds, WindowOptions, canvas, div,
    prelude::*, px, rgb, size,
};
use image::{Frame, RgbaImage};
use net::tap::NetworkTapEvent;
use rever_core::input::w3c_key_name;
use rever_core::traffic::{self, Headers, ListFilter, RequestPatch, TrafficStore};
use servo::{
    Code, InputEvent, Key, KeyState, KeyboardEvent, Location, Modifiers, MouseButtonAction,
    MouseButtonEvent, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
};
use url::Url;

const DEFAULT_URL: &str = "https://example.com/";
const SIDE_PANEL_WIDTH: f32 = 380.0;

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

struct Browser {
    engine: Engine,
    /// The latest page frame, in the BGRA layout GPUI paints.
    frame: Option<Arc<RenderImage>>,
    /// The same frame as RGBA, kept only for --self-check.
    last_rgba: Option<RgbaImage>,
    title: SharedString,
    url: SharedString,
    /// Where the page was laid out last, in window coordinates. Written during paint.
    page_bounds: Rc<Cell<Bounds<Pixels>>>,
    traffic: Arc<Mutex<TrafficStore>>,
    focus: FocusHandle,
    self_check: Option<String>,
}

impl Browser {
    fn new(
        url: Url,
        self_check: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (wake, mut woken) = futures::channel::mpsc::unbounded::<()>();
        let traffic = Arc::new(Mutex::new(TrafficStore::default()));
        {
            let (store, wake) = (traffic.clone(), wake.clone());
            net::tap::set_network_tap(move |event| {
                record(&store, event);
                let _ = wake.unbounded_send(());
            });
        }
        let engine = Engine::new(
            waker(wake),
            PhysicalSize::new(800, 600),
            window.scale_factor(),
            url.clone(),
        )
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

        Self {
            engine,
            frame: None,
            last_rgba: None,
            title: "".into(),
            url: url.to_string().into(),
            page_bounds: Rc::new(Cell::new(Bounds::default())),
            traffic,
            focus: cx.focus_handle(),
            self_check,
        }
    }

    fn pump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut frame_ready = false;
        for event in self.engine.pump() {
            match event {
                EngineEvent::FrameReady => frame_ready = true,
                EngineEvent::Title(title) => self.title = title.into(),
                EngineEvent::Url(url) => self.url = url.into(),
                EngineEvent::Console { .. } => {}
            }
        }
        if frame_ready {
            self.capture(window);
        }
        cx.notify();
    }

    fn capture(&mut self, window: &mut Window) {
        let Some(mut image) = self.engine.frame() else {
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
        if let Some(old) = self.frame.replace(frame) {
            let _ = window.drop_image(old);
        }
    }

    /// Make the page as large as the area the interface gave it.
    fn fit_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = self.page_bounds.get();
        let scale = window.scale_factor();
        let device = |length: Pixels| (f32::from(length) * scale).round().max(1.0) as u32;
        let size = PhysicalSize::new(device(bounds.size.width), device(bounds.size.height));
        if size != self.engine.size() {
            self.engine.resize(size);
            self.pump(window, cx);
        }
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
        window.focus(&self.focus);
        let point = self.page_point(event.position);
        self.engine
            .input(InputEvent::MouseMove(servo::MouseMoveEvent::new(point)));
        self.engine
            .input(InputEvent::MouseButton(MouseButtonEvent::new(
                MouseButtonAction::Down,
                servo::MouseButton::Primary,
                point,
            )));
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.engine
            .input(InputEvent::MouseButton(MouseButtonEvent::new(
                MouseButtonAction::Up,
                servo::MouseButton::Primary,
                self.page_point(event.position),
            )));
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        let point = self.page_point(event.position);
        self.engine
            .input(InputEvent::MouseMove(servo::MouseMoveEvent::new(point)));
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
        self.engine.input(InputEvent::Wheel(WheelEvent::new(
            WheelDelta { x, y, z: 0.0, mode },
            self.page_point(event.position),
        )));
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
        self.engine
            .input(InputEvent::Keyboard(KeyboardEvent::new_without_event(
                state,
                key,
                Code::Unidentified,
                Location::Standard,
                modifiers,
                false,
                false,
            )));
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
        let state = serde_json::json!({
            "title": self.title.to_string(),
            "url": self.url.to_string(),
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

fn waker(wake: UnboundedSender<()>) -> impl Fn() + Send + Sync + 'static {
    move || {
        let _ = wake.unbounded_send(());
    }
}

impl Render for Browser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.frame.clone();
        let bounds_cell = self.page_bounds.clone();
        let this = cx.entity();
        let rows: Vec<(String, String, String)> = {
            let store = self.traffic.lock().unwrap();
            store
                .list(&ListFilter {
                    limit: Some(60),
                    ..Default::default()
                })
                .iter()
                .map(|r| {
                    let status = r.status.map_or("…".to_owned(), |status| status.to_string());
                    (r.method.clone(), status, r.url.clone())
                })
                .collect()
        };

        let page = canvas(
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
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x1f2328))
            .text_sm()
            .child(
                div()
                    .h(px(40.0))
                    .px_3()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_3()
                    .bg(rgb(0xdfe3e8))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .child(div().text_color(rgb(0x57606a)).child(self.url.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("page")
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .track_focus(&self.focus)
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
                            .child(page),
                    )
                    .child(
                        div()
                            .id("traffic")
                            .w(px(SIDE_PANEL_WIDTH))
                            .h_full()
                            .flex_none()
                            .overflow_hidden()
                            .border_l_1()
                            .border_color(rgb(0xd0d7de))
                            .bg(rgb(0xfbfbfc))
                            .child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(format!("Traffic · {}", rows.len())),
                            )
                            .children(rows.into_iter().map(|(method, status, url)| {
                                div()
                                    .px_3()
                                    .py_1()
                                    .flex()
                                    .gap_2()
                                    .text_xs()
                                    .child(div().w(px(40.0)).flex_none().child(method))
                                    .child(div().w(px(30.0)).flex_none().child(status))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .child(url),
                                    )
                            })),
                    ),
            )
    }
}

fn main() {
    let mut url = DEFAULT_URL.to_owned();
    let mut self_check = None;
    for arg in std::env::args().skip(1) {
        match arg.strip_prefix("--self-check=") {
            Some(dir) => self_check = Some(dir.to_owned()),
            None => url = arg,
        }
    }
    let url = Url::parse(&url).expect("the URL argument is not a valid URL");

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Browser::new(url, self_check, window, cx)),
        )
        .expect("could not open the main window");
        cx.activate(true);
    });
}
