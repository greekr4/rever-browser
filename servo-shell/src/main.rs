// Spike: one Servo WebView in a winit window, based on Servo's own
// `components/servo/examples/winit_minimal.rs` (MPL-2.0).
//
// Usage: rever-servo-shell [--cdp-port=PORT] [URL]
//   --cdp-port=PORT     serve the Chrome DevTools Protocol subset in cdp.rs, so
//                       rever's external-browser mode can attach to this shell.
//   REVER_SPIKE_EXIT=1  exit once the page has loaded and the script result is printed.
//
// Without --cdp-port the hooks are printed as one JSON-ish line each on stdout.
//
// shortcut: the window itself only forwards wheel input (as in the upstream
// example); clicks and keys arrive over CDP. Forward winit mouse/keyboard
// events before using the window as an interactive browser.

mod cdp;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use euclid::{Point2D, Scale};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ExtendedColorType};
use net::tap::NetworkTapEvent;
use serde_json::{Value, json};
use servo::{
    ConsoleLogLevel, DevicePoint, EventLoopWaker, InputEvent, Key, KeyState, KeyboardEvent,
    LoadStatus, MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent, RenderingContext,
    Servo, ServoBuilder, UserContentManager, UserScript, WebResourceLoad, WebView, WebViewBuilder,
    WebViewDelegate, WebViewPoint, WheelDelta, WheelEvent, WheelMode, WindowRenderingContext,
};
use url::Url;
use winit::application::ApplicationHandler;
use winit::event::{MouseScrollDelta, WindowEvent};
use winit::event_loop::EventLoop;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

const DEFAULT_URL: &str = "https://example.com/";
/// Screencast frames are sent at most this often.
const FRAME_INTERVAL: Duration = Duration::from_millis(150);

type Cdp = (Arc<cdp::Hub>, Receiver<cdp::MainCommand>);

fn main() -> Result<(), Box<dyn Error>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install crypto provider");

    let mut url = DEFAULT_URL.to_owned();
    let mut cdp_port = None;
    for arg in std::env::args().skip(1) {
        match arg.strip_prefix("--cdp-port=") {
            Some(port) => cdp_port = Some(port.parse::<u16>()?),
            None => url = arg,
        }
    }
    let url = Url::parse(&url)?;

    let event_loop = EventLoop::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let cdp = match cdp_port {
        Some(port) => {
            let wake = proxy.clone();
            Some(cdp::start(port, move || {
                let _ = wake.send_event(WakerEvent);
            })?)
        }
        None => None,
    };

    // Patched servo-net only (see vendor/servo-net/tap.rs): bodies the embedding API does not expose.
    let hub = cdp.as_ref().map(|(hub, _)| hub.clone());
    net::tap::set_network_tap(move |event| forward_tap(hub.as_deref(), event));

    let mut app = App::Initial(Waker(proxy), url, cdp);
    Ok(event_loop.run_app(&mut app)?)
}

/// Runs on Servo's network threads.
fn forward_tap(hub: Option<&cdp::Hub>, event: NetworkTapEvent) {
    fn headers_json<'a>(headers: impl Iterator<Item = (&'a str, &'a [u8])>) -> Value {
        let mut map = serde_json::Map::new();
        for (name, value) in headers {
            let value = String::from_utf8_lossy(value).into_owned();
            // CDP joins repeated headers with a newline.
            match map.get_mut(name) {
                Some(Value::String(existing)) => *existing = format!("{existing}\n{value}"),
                _ => {
                    map.insert(name.to_owned(), Value::String(value));
                }
            }
        }
        Value::Object(map)
    }
    let preview = |body: &[u8]| String::from_utf8_lossy(&body[..body.len().min(80)]).into_owned();

    match event {
        NetworkTapEvent::Request {
            id,
            url,
            method,
            headers,
            body,
            destination,
        } => match hub {
            Some(hub) => hub.on_request(
                &id,
                url.as_str(),
                method.as_str(),
                headers_json(
                    headers
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_bytes())),
                ),
                &body,
                &destination,
            ),
            None => println!(
                "{{\"type\":\"tap-request\",\"method\":{:?},\"url\":{:?},\"headers\":{},\"bodyBytes\":{},\"body\":{:?}}}",
                method.as_str(),
                url.as_str(),
                headers.len(),
                body.len(),
                preview(&body)
            ),
        },
        NetworkTapEvent::Response {
            id,
            url,
            status,
            headers,
            body,
        } => match hub {
            Some(hub) => hub.on_response(
                &id,
                url.as_str(),
                status,
                headers_json(
                    headers
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_bytes())),
                ),
                body,
            ),
            None => println!(
                "{{\"type\":\"tap-response\",\"status\":{status},\"url\":{:?},\"headers\":{},\"bodyBytes\":{},\"body\":{:?}}}",
                url.as_str(),
                headers.len(),
                body.len(),
                preview(&body)
            ),
        },
    }
}

struct AppState {
    window: Window,
    servo: Servo,
    rendering_context: Rc<WindowRenderingContext>,
    webviews: RefCell<Vec<WebView>>,
    user_content: Rc<UserContentManager>,
    cdp: Option<Cdp>,
    screencast: Cell<bool>,
    last_frame: Cell<Instant>,
    frame_session: Cell<u32>,
}

impl AppState {
    /// Run the CDP commands queued by the connection threads.
    fn run_cdp_commands(&self) {
        let Some((_, queue)) = &self.cdp else { return };
        let webviews = self.webviews.borrow();
        let Some(webview) = webviews.last() else {
            return;
        };
        while let Ok(command) = queue.try_recv() {
            match command {
                cdp::MainCommand::Navigate { url, reply } => match Url::parse(&url) {
                    Ok(url) => {
                        webview.load(url);
                        reply.ok(json!({ "frameId": "main", "loaderId": "" }));
                    }
                    Err(error) => reply.error(format!("Cannot navigate to invalid URL: {error}")),
                },
                // shortcut: no awaitPromise — a Promise result comes back as an empty object.
                cdp::MainCommand::Evaluate { expression, reply } => {
                    webview.evaluate_javascript(expression, move |result| match result {
                        Ok(value) => reply.ok(json!({ "result": cdp::remote_object(&value) })),
                        Err(error) => reply.ok(json!({
                            "result": { "type": "undefined" },
                            "exceptionDetails": { "text": format!("{error:?}") },
                        })),
                    });
                }
                cdp::MainCommand::AddInitScript { source, reply } => {
                    let script = Rc::new(UserScript::new(source, None));
                    self.user_content.add_script(script);
                    reply.ok(json!({ "identifier": "1" }));
                }
                cdp::MainCommand::Screencast { on, reply } => {
                    self.screencast.set(on);
                    reply.ok(json!({}));
                    self.send_frame(webview, true);
                }
                cdp::MainCommand::Mouse { params, reply } => {
                    for event in mouse_events(&params, self.window.scale_factor()) {
                        webview.notify_input_event(event);
                    }
                    reply.ok(json!({}));
                }
                cdp::MainCommand::Key { params, reply } => {
                    if let Some(event) = key_event(&params) {
                        webview.notify_input_event(event);
                    }
                    reply.ok(json!({}));
                }
            }
        }
    }

    /// Capture the WebView and send it as a `Page.screencastFrame` JPEG.
    fn send_frame(&self, webview: &WebView, force: bool) {
        let Some((hub, _)) = &self.cdp else { return };
        if !self.screencast.get() || (!force && self.last_frame.get().elapsed() < FRAME_INTERVAL) {
            return;
        }
        self.last_frame.set(Instant::now());
        self.frame_session.set(self.frame_session.get() + 1);
        let (hub, session) = (hub.clone(), self.frame_session.get());
        let scale = self.window.scale_factor();
        webview.take_screenshot(None, move |result| {
            let Ok(image) = result else { return };
            let (width, height) = image.dimensions();
            let rgb = DynamicImage::ImageRgba8(image).to_rgb8();
            let mut jpeg = Vec::new();
            if JpegEncoder::new_with_quality(&mut jpeg, 70)
                .encode(&rgb, width, height, ExtendedColorType::Rgb8)
                .is_err()
            {
                return;
            }
            hub.broadcast(
                "Page.screencastFrame",
                json!({
                    "data": BASE64.encode(jpeg),
                    "metadata": {
                        "offsetTop": 0,
                        "pageScaleFactor": 1,
                        "deviceWidth": f64::from(width) / scale,
                        "deviceHeight": f64::from(height) / scale,
                        "scrollOffsetX": 0,
                        "scrollOffsetY": 0,
                        "timestamp": hub.timestamp(),
                    },
                    "sessionId": session,
                }),
            );
        });
    }
}

/// `Input.dispatchMouseEvent` params → Servo input events. CDP coordinates and
/// wheel deltas are CSS pixels; Servo's pixel wheel deltas are device pixels.
fn mouse_events(params: &Value, scale: f64) -> Vec<InputEvent> {
    let number = |name: &str| params[name].as_f64().unwrap_or_default();
    let point = WebViewPoint::Page(Point2D::new(number("x") as f32, number("y") as f32));
    let button = match params["button"].as_str() {
        Some("middle") => MouseButton::Auxiliary,
        Some("right") => MouseButton::Secondary,
        _ => MouseButton::Primary,
    };
    let press = |action| InputEvent::MouseButton(MouseButtonEvent::new(action, button, point));
    let moved = InputEvent::MouseMove(MouseMoveEvent::new(point));
    match params["type"].as_str() {
        // Move first so the press lands on the element under the new position.
        Some("mousePressed") => vec![moved, press(MouseButtonAction::Down)],
        Some("mouseReleased") => vec![press(MouseButtonAction::Up)],
        Some("mouseMoved") => vec![moved],
        // CDP: positive delta scrolls down/right. Servo: positive scrolls up/left.
        Some("mouseWheel") => vec![InputEvent::Wheel(WheelEvent::new(
            WheelDelta {
                x: -number("deltaX") * scale,
                y: -number("deltaY") * scale,
                z: 0.0,
                mode: WheelMode::DeltaPixel,
            },
            point,
        ))],
        _ => vec![],
    }
}

/// `Input.dispatchKeyEvent` params → a Servo keyboard event. `char` events are
/// skipped: Servo derives the typed text from the key-down itself.
fn key_event(params: &Value) -> Option<InputEvent> {
    let state = match params["type"].as_str()? {
        "keyDown" | "rawKeyDown" => KeyState::Down,
        "keyUp" => KeyState::Up,
        _ => return None,
    };
    let name = [params["key"].as_str(), params["text"].as_str()]
        .into_iter()
        .flatten()
        .find(|name| !name.is_empty())?;
    let key = Key::from_str(name).ok()?;
    Some(InputEvent::Keyboard(KeyboardEvent::from_state_and_key(
        state, key,
    )))
}

impl WebViewDelegate for AppState {
    fn notify_new_frame_ready(&self, webview: WebView) {
        self.window.request_redraw();
        self.send_frame(&webview, false);
    }

    fn notify_page_title_changed(&self, _: WebView, title: Option<String>) {
        self.window
            .set_title(title.as_deref().unwrap_or("rever-servo-shell"));
        if let Some((hub, _)) = &self.cdp {
            hub.set_page(None, Some(title.unwrap_or_default()));
        }
    }

    fn notify_url_changed(&self, _: WebView, url: Url) {
        if let Some((hub, _)) = &self.cdp {
            hub.set_page(Some(url.into()), None);
        }
    }

    // Every HTTP request the page makes passes through here. Dropping `load`
    // without calling `intercept` lets the request continue. With CDP on, the
    // tap reports the same request with its body, so this stays quiet.
    fn load_web_resource(&self, _: WebView, load: WebResourceLoad) {
        if self.cdp.is_some() {
            return;
        }
        let r = &load.request;
        println!(
            "{{\"type\":\"request\",\"method\":{:?},\"url\":{:?},\"destination\":\"{:?}\",\"mainFrame\":{},\"headers\":{}}}",
            r.method.as_str(),
            r.url.as_str(),
            r.destination,
            r.is_for_main_frame,
            r.headers.len()
        );
    }

    fn show_console_message(&self, _: WebView, level: ConsoleLogLevel, message: String) {
        match &self.cdp {
            Some((hub, _)) => hub.broadcast(
                "Runtime.consoleAPICalled",
                json!({
                    "type": format!("{level:?}").to_lowercase(),
                    "args": [{ "type": "string", "value": message }],
                    "executionContextId": 1,
                    "timestamp": hub.timestamp() * 1000.0,
                }),
            ),
            None => {
                println!("{{\"type\":\"console\",\"level\":\"{level:?}\",\"message\":{message:?}}}")
            }
        }
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        if status != LoadStatus::Complete || self.cdp.is_some() {
            return;
        }
        webview.evaluate_javascript(
            "document.title + ' | links=' + document.links.length",
            |result| {
                println!(
                    "{{\"type\":\"evaluate\",\"result\":{:?}}}",
                    format!("{result:?}")
                );
                if std::env::var_os("REVER_SPIKE_EXIT").is_some() {
                    std::process::exit(0);
                }
            },
        );
    }
}

enum App {
    Initial(Waker, Url, Option<Cdp>),
    Running(Rc<AppState>),
}

impl ApplicationHandler<WakerEvent> for App {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let Self::Initial(waker, url, cdp) = self else {
            return;
        };
        let display_handle = event_loop
            .display_handle()
            .expect("Failed to get display handle");
        let window = event_loop
            .create_window(Window::default_attributes().with_title("rever-servo-shell"))
            .expect("Failed to create winit Window");
        let window_handle = window.window_handle().expect("Failed to get window handle");

        let rendering_context = Rc::new(
            WindowRenderingContext::new(display_handle, window_handle, window.inner_size())
                .expect("Could not create RenderingContext for window."),
        );
        let _ = rendering_context.make_current();

        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(waker.clone()))
            .build();
        servo.setup_logging();
        let user_content = Rc::new(UserContentManager::new(&servo));

        let app_state = Rc::new(AppState {
            window,
            servo,
            rendering_context,
            webviews: Default::default(),
            user_content: user_content.clone(),
            cdp: cdp.take(),
            screencast: Cell::new(false),
            last_frame: Cell::new(Instant::now()),
            frame_session: Cell::new(0),
        });
        let webview = WebViewBuilder::new(&app_state.servo, app_state.rendering_context.clone())
            .url(url.clone())
            .hidpi_scale_factor(Scale::new(app_state.window.scale_factor() as f32))
            .user_content_manager(user_content)
            .delegate(app_state.clone())
            .build();
        app_state.webviews.borrow_mut().push(webview);
        *self = Self::Running(app_state);
    }

    fn user_event(&mut self, _: &winit::event_loop::ActiveEventLoop, _: WakerEvent) {
        if let Self::Running(state) = self {
            state.servo.spin_event_loop();
            state.run_cdp_commands();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let Self::Running(state) = self else { return };
        state.servo.spin_event_loop();
        state.run_cdp_commands();
        let webviews = state.webviews.borrow();
        let Some(webview) = webviews.last() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                webview.paint();
                state.rendering_context.present();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y, mode) = match delta {
                    MouseScrollDelta::LineDelta(dx, dy) => {
                        ((dx * 76.0) as f64, (dy * 76.0) as f64, WheelMode::DeltaLine)
                    }
                    MouseScrollDelta::PixelDelta(delta) => {
                        (delta.x, delta.y, WheelMode::DeltaPixel)
                    }
                };
                webview.notify_input_event(InputEvent::Wheel(WheelEvent::new(
                    WheelDelta { x, y, z: 0.0, mode },
                    DevicePoint::default().into(),
                )));
            }
            WindowEvent::Resized(new_size) => webview.resize(new_size),
            _ => (),
        }
    }
}

#[derive(Clone)]
struct Waker(winit::event_loop::EventLoopProxy<WakerEvent>);
#[derive(Debug)]
struct WakerEvent;

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        if let Err(error) = self.0.send_event(WakerEvent) {
            eprintln!("Failed to wake event loop: {error:?}");
        }
    }
}
