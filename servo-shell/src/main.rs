// Spike: a Servo-based shell for rever, grown from Servo's own
// `components/servo/examples/winit_minimal.rs` (MPL-2.0).
//
// Usage: rever-servo-shell [--cdp-port=PORT] [--ui=URL [--ui-init-script=FILE]] [URL]
//   URL                    the page to browse (default: https://example.com/).
//   --cdp-port=PORT        serve the Chrome DevTools Protocol subset in cdp.rs for the page.
//   --ui=URL               also load rever's own interface in the same window. The page is
//                          drawn where the interface puts its <webview> element.
//   --ui-init-script=FILE  script run in the interface before its own code (dev/rev-stub.js).
//   --snapshot=FILE        save the window as a PNG after 6 seconds and exit (for checks).
//   REVER_SPIKE_EXIT=1     exit once the page has loaded and the script result is printed.
//
// Without --cdp-port the page's hooks are printed as one JSON-ish line each on stdout.

mod cdp;
mod input;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use euclid::Scale;
use euclid::default::{Point2D, Rect, Size2D};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ExtendedColorType};
use net::tap::NetworkTapEvent;
use serde_json::{Value, json};
use servo::{
    ConsoleLogLevel, DevicePoint, EventLoopWaker, InputEvent, JSValue, LoadStatus,
    OffscreenRenderingContext, RenderingContext, Servo, ServoBuilder, UserContentManager,
    UserScript, WebResourceLoad, WebView, WebViewBuilder, WebViewDelegate, WindowRenderingContext,
};
use url::Url;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::EventLoop;
use winit::keyboard::ModifiersState;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

const DEFAULT_URL: &str = "https://example.com/";
/// Screencast frames are sent at most this often.
const FRAME_INTERVAL: Duration = Duration::from_millis(150);
/// How often the interface is asked where it wants the page drawn.
const LAYOUT_INTERVAL: Duration = Duration::from_millis(200);
/// The interface marks the page area with its <webview> element (unknown to Servo, so it
/// may be zero-sized; then its parent box is used). Result: "x,y,width,height" in CSS px.
const PAGE_RECT_SCRIPT: &str = "(() => { const el = document.querySelector('webview'); if (!el) return ''; \
    let r = el.getBoundingClientRect(); if (!r.width || !r.height) r = el.parentElement.getBoundingClientRect(); \
    return [r.x, r.y, r.width, r.height].join(','); })()";

type Cdp = (Arc<cdp::Hub>, Receiver<cdp::MainCommand>);

struct Options {
    snapshot: Option<String>,
    page_url: Url,
    ui_url: Option<Url>,
    ui_init_script: Option<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install crypto provider");

    let mut page_url = DEFAULT_URL.to_owned();
    let (mut cdp_port, mut ui_url, mut ui_init_script, mut snapshot) = (None, None, None, None);
    for arg in std::env::args().skip(1) {
        if let Some(port) = arg.strip_prefix("--cdp-port=") {
            cdp_port = Some(port.parse::<u16>()?);
        } else if let Some(url) = arg.strip_prefix("--ui=") {
            ui_url = Some(Url::parse(url)?);
        } else if let Some(path) = arg.strip_prefix("--snapshot=") {
            snapshot = Some(path.to_owned());
        } else if let Some(path) = arg.strip_prefix("--ui-init-script=") {
            ui_init_script = Some(std::fs::read_to_string(path)?);
        } else {
            page_url = arg;
        }
    }
    let options = Options {
        snapshot,
        page_url: Url::parse(&page_url)?,
        ui_url,
        ui_init_script,
    };

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
    // shortcut: the tap is process-wide, so with --ui the interface's own requests are reported
    // too; tag tap events with the WebView before relying on this for the embedded browser.
    let hub = cdp.as_ref().map(|(hub, _)| hub.clone());
    net::tap::set_network_tap(move |event| forward_tap(hub.as_deref(), event));

    let mut app = App::Initial(Waker(proxy), options, cdp);
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

/// One WebView and the offscreen surface it paints into.
struct View {
    webview: WebView,
    context: Rc<OffscreenRenderingContext>,
}

struct AppState {
    me: Weak<AppState>,
    window: Window,
    servo: Servo,
    window_context: Rc<WindowRenderingContext>,
    /// The page being browsed. `None` only while the state is being built.
    page: RefCell<Option<View>>,
    /// rever's own interface, when started with --ui.
    ui: RefCell<Option<View>>,
    /// Where the page is drawn: device pixels, origin at the window's top-left.
    page_rect: Cell<Rect<i32>>,
    last_layout: Cell<Instant>,
    cursor: Cell<(f64, f64)>,
    modifiers: Cell<ModifiersState>,
    snapshot: Option<String>,
    started: Instant,
    /// Which view the keyboard goes to: the one clicked last.
    keys_to_page: Cell<bool>,
    page_scripts: Rc<UserContentManager>,
    cdp: Option<Cdp>,
    screencast: Cell<bool>,
    last_frame: Cell<Instant>,
    frame_session: Cell<u32>,
}

impl AppState {
    fn page_webview(&self) -> Option<WebView> {
        self.page.borrow().as_ref().map(|view| view.webview.clone())
    }

    fn is_page(&self, webview: &WebView) -> bool {
        self.page_webview()
            .is_some_and(|page| page.id() == webview.id())
    }

    fn window_rect(&self) -> Rect<i32> {
        let size = self.window.inner_size();
        Rect::new(
            Point2D::origin(),
            Size2D::new(size.width as i32, size.height as i32),
        )
    }

    /// Paint both views into their surfaces and copy them onto the window.
    fn draw(&self) {
        let window_height = self.window.inner_size().height as i32;
        let views = [
            (&self.ui, self.window_rect()),
            (&self.page, self.page_rect.get()),
        ];
        for (view, _) in &views {
            if let Some(view) = view.borrow().as_ref() {
                view.webview.paint();
            }
        }
        if self.window_context.make_current().is_err() {
            return;
        }
        // Bind the window's framebuffer first: each blit starts by clearing its target area in
        // whichever framebuffer is bound, which would otherwise be the view painted last.
        self.window_context.prepare_for_rendering();
        let gl = self.window_context.glow_gl_api();
        for (view, rect) in views {
            let view = view.borrow();
            let Some(view) = view.as_ref() else { continue };
            if rect.is_empty() {
                continue;
            }
            // GL's origin is the bottom-left corner.
            let target = Rect::new(
                Point2D::new(
                    rect.origin.x,
                    window_height - rect.origin.y - rect.size.height,
                ),
                rect.size,
            );
            if let Some(blit) = view.context.render_to_parent_callback() {
                blit(&gl, target);
            }
        }
        self.save_snapshot_if_due();
        self.window_context.present();
    }

    /// `--snapshot`: write what was just drawn to a PNG, then exit.
    fn save_snapshot_if_due(&self) {
        let Some(path) = &self.snapshot else { return };
        if self.started.elapsed() < Duration::from_secs(6) {
            // Keep drawing until the snapshot is due, even if nothing on screen changes.
            self.window.request_redraw();
            return;
        }
        let size = self.window.inner_size();
        let area = servo::DeviceIntRect::from_size(servo::DeviceIntSize::new(
            size.width as i32,
            size.height as i32,
        ));
        let saved = self
            .window_context
            .read_to_image(area)
            .is_some_and(|image| image.save(path).is_ok());
        std::process::exit(if saved { 0 } else { 1 });
    }

    /// Ask the interface where the page goes, and resize the page to fit.
    fn update_layout(&self) {
        let ui = self.ui.borrow();
        let Some(ui) = ui.as_ref() else { return };
        if self.last_layout.get().elapsed() < LAYOUT_INTERVAL {
            return;
        }
        self.last_layout.set(Instant::now());
        let (me, scale) = (self.me.clone(), self.window.scale_factor());
        ui.webview
            .evaluate_javascript(PAGE_RECT_SCRIPT, move |result| {
                let (Some(state), Ok(JSValue::String(text))) = (me.upgrade(), result) else {
                    return;
                };
                let numbers: Vec<f64> = text
                    .split(',')
                    .filter_map(|part| part.parse().ok())
                    .collect();
                let &[x, y, width, height] = &numbers[..] else {
                    return;
                };
                let device = |css: f64| (css * scale).round() as i32;
                let rect = Rect::new(
                    Point2D::new(device(x), device(y)),
                    Size2D::new(device(width), device(height)),
                );
                state.set_page_rect(rect);
            });
    }

    fn set_page_rect(&self, rect: Rect<i32>) {
        if rect == self.page_rect.get() {
            return;
        }
        self.page_rect.set(rect);
        if let Some(page) = self.page.borrow().as_ref() {
            if !rect.is_empty() {
                let size = PhysicalSize::new(rect.size.width as u32, rect.size.height as u32);
                page.webview.resize(size);
            }
        }
        self.window.request_redraw();
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        self.window_context.resize(size);
        match self.ui.borrow().as_ref() {
            Some(ui) => {
                ui.webview.resize(size);
            }
            // Without an interface the page fills the window.
            None => self.set_page_rect(Rect::new(
                Point2D::origin(),
                Size2D::new(size.width as i32, size.height as i32),
            )),
        }
    }

    /// Send a window input event to the view under the cursor (or the focused one for keys).
    fn route_input(&self, event: impl FnOnce(DevicePoint) -> Option<InputEvent>, by_cursor: bool) {
        let (x, y) = self.cursor.get();
        let rect = self.page_rect.get();
        let over_page = rect.contains(Point2D::new(x as i32, y as i32));
        let to_page = self.ui.borrow().is_none()
            || if by_cursor {
                over_page
            } else {
                self.keys_to_page.get()
            };
        let (view, origin) = if to_page {
            (&self.page, rect.origin)
        } else {
            (&self.ui, Point2D::origin())
        };
        let point = DevicePoint::new(x as f32 - origin.x as f32, y as f32 - origin.y as f32);
        if let (Some(view), Some(event)) = (view.borrow().as_ref(), event(point)) {
            view.webview.notify_input_event(event);
        }
    }

    fn focus_view_under_cursor(&self) {
        let (x, y) = self.cursor.get();
        let on_page = self.ui.borrow().is_none()
            || self
                .page_rect
                .get()
                .contains(Point2D::new(x as i32, y as i32));
        self.keys_to_page.set(on_page);
        if let Some(page) = self.page.borrow().as_ref() {
            page.webview.set_focused(on_page);
        }
        if let Some(ui) = self.ui.borrow().as_ref() {
            ui.webview.set_focused(!on_page);
        }
    }

    /// Run the CDP commands queued by the connection threads. They address the page.
    fn run_cdp_commands(&self) {
        let Some((_, queue)) = &self.cdp else { return };
        let Some(webview) = self.page_webview() else {
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
                    self.page_scripts
                        .add_script(Rc::new(UserScript::new(source, None)));
                    reply.ok(json!({ "identifier": "1" }));
                }
                cdp::MainCommand::Screencast { on, reply } => {
                    self.screencast.set(on);
                    reply.ok(json!({}));
                    self.send_frame(&webview, true);
                }
                cdp::MainCommand::Mouse { params, reply } => {
                    for event in input::cdp_mouse(&params, self.window.scale_factor()) {
                        webview.notify_input_event(event);
                    }
                    reply.ok(json!({}));
                }
                cdp::MainCommand::Key { params, reply } => {
                    if let Some(event) = input::cdp_key(&params) {
                        webview.notify_input_event(event);
                    }
                    reply.ok(json!({}));
                }
            }
        }
    }

    /// Capture the page and send it as a `Page.screencastFrame` JPEG.
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

impl WebViewDelegate for AppState {
    fn notify_new_frame_ready(&self, webview: WebView) {
        self.window.request_redraw();
        if self.is_page(&webview) {
            self.send_frame(&webview, false);
        } else {
            self.update_layout();
        }
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        if !self.is_page(&webview) {
            return;
        }
        self.window
            .set_title(title.as_deref().unwrap_or("rever-servo-shell"));
        if let Some((hub, _)) = &self.cdp {
            hub.set_page(None, Some(title.unwrap_or_default()));
        }
    }

    fn notify_url_changed(&self, webview: WebView, url: Url) {
        if let (true, Some((hub, _))) = (self.is_page(&webview), &self.cdp) {
            hub.set_page(Some(url.into()), None);
        }
    }

    // Every HTTP request a view makes passes through here. Dropping `load`
    // without calling `intercept` lets the request continue. With CDP on, the
    // tap reports the same request with its body, so this stays quiet.
    fn load_web_resource(&self, webview: WebView, load: WebResourceLoad) {
        if self.cdp.is_some() || !self.is_page(&webview) {
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

    fn show_console_message(&self, webview: WebView, level: ConsoleLogLevel, message: String) {
        if !self.is_page(&webview) {
            // The interface's own console: errors are worth seeing while it is being ported.
            if matches!(level, ConsoleLogLevel::Error) {
                eprintln!("[ui] {message}");
            }
            return;
        }
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
        if status != LoadStatus::Complete || self.cdp.is_some() || !self.is_page(&webview) {
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
    Initial(Waker, Options, Option<Cdp>),
    Running(Rc<AppState>),
}

impl ApplicationHandler<WakerEvent> for App {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let Self::Initial(waker, options, cdp) = self else {
            return;
        };
        let display_handle = event_loop
            .display_handle()
            .expect("Failed to get display handle");
        let window = event_loop
            .create_window(
                Window::default_attributes()
                    .with_title("rever-servo-shell")
                    .with_inner_size(LogicalSize::new(1280.0, 800.0)),
            )
            .expect("Failed to create winit Window");
        let window_handle = window.window_handle().expect("Failed to get window handle");
        let size = window.inner_size();

        let window_context = Rc::new(
            WindowRenderingContext::new(display_handle, window_handle, size)
                .expect("Could not create RenderingContext for window."),
        );
        let _ = window_context.make_current();

        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(waker.clone()))
            .build();
        servo.setup_logging();
        let page_scripts = Rc::new(UserContentManager::new(&servo));
        let scale = Scale::new(window.scale_factor() as f32);
        let has_ui = options.ui_url.is_some();

        let state = Rc::new_cyclic(|me| AppState {
            me: me.clone(),
            window,
            servo,
            window_context,
            page: RefCell::new(None),
            ui: RefCell::new(None),
            // With an interface, the page stays hidden until the interface says where it goes.
            page_rect: Cell::new(if has_ui {
                Rect::zero()
            } else {
                Rect::new(
                    Point2D::origin(),
                    Size2D::new(size.width as i32, size.height as i32),
                )
            }),
            last_layout: Cell::new(Instant::now() - LAYOUT_INTERVAL),
            cursor: Cell::new((0.0, 0.0)),
            modifiers: Cell::new(ModifiersState::empty()),
            snapshot: options.snapshot.clone(),
            started: Instant::now(),
            keys_to_page: Cell::new(!has_ui),
            page_scripts: page_scripts.clone(),
            cdp: cdp.take(),
            screencast: Cell::new(false),
            last_frame: Cell::new(Instant::now()),
            frame_session: Cell::new(0),
        });

        let build = |url: &Url, scripts: Rc<UserContentManager>| {
            let context = Rc::new(state.window_context.offscreen_context(size));
            let webview = WebViewBuilder::new(&state.servo, context.clone())
                .url(url.clone())
                .hidpi_scale_factor(scale)
                .user_content_manager(scripts)
                .delegate(state.clone())
                .build();
            View { webview, context }
        };
        if let Some(ui_url) = &options.ui_url {
            let ui_scripts = Rc::new(UserContentManager::new(&state.servo));
            if let Some(source) = &options.ui_init_script {
                ui_scripts.add_script(Rc::new(UserScript::new(source.clone(), None)));
            }
            *state.ui.borrow_mut() = Some(build(ui_url, ui_scripts));
        }
        *state.page.borrow_mut() = Some(build(&options.page_url, page_scripts));
        *self = Self::Running(state);
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

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => state.draw(),
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::ModifiersChanged(modifiers) => state.modifiers.set(modifiers.state()),
            WindowEvent::CursorMoved { position, .. } => {
                state.cursor.set((position.x, position.y));
                state.route_input(|point| Some(input::mouse_move(point)), true);
            }
            WindowEvent::MouseInput {
                state: pressed,
                button,
                ..
            } => {
                if pressed == ElementState::Pressed {
                    state.focus_view_under_cursor();
                }
                state.route_input(
                    |point| Some(input::mouse_button(pressed, button, point)),
                    true,
                );
            }
            WindowEvent::MouseWheel { delta, .. } => {
                state.route_input(|point| Some(input::wheel(delta, point)), true);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let modifiers = state.modifiers.get();
                state.route_input(|_| input::key(&event, modifiers), false);
            }
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
