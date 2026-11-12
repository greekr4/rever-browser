// Spike: one Servo WebView in a winit window, based on Servo's own
// `components/servo/examples/winit_minimal.rs` (MPL-2.0). Added on top: the
// three hooks rever gets from CDP today — request capture, console capture and
// script evaluation — printed as one JSON-ish line each on stdout.
//
// Usage: rever-servo-shell [URL]
//   REVER_SPIKE_EXIT=1  exit once the page has loaded and the script result is printed.
//
// shortcut: wheel is the only input wired up (as in the upstream example);
// add mouse/keyboard forwarding before using this as an interactive shell.

use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;

use euclid::Scale;
use servo::{
    ConsoleLogLevel, DevicePoint, EventLoopWaker, InputEvent, LoadStatus, RenderingContext, Servo,
    ServoBuilder, WebResourceLoad, WebView, WebViewBuilder, WebViewDelegate, WheelDelta,
    WheelEvent, WheelMode, WindowRenderingContext,
};
use url::Url;
use winit::application::ApplicationHandler;
use winit::event::{MouseScrollDelta, WindowEvent};
use winit::event_loop::EventLoop;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

const DEFAULT_URL: &str = "https://example.com/";

fn main() -> Result<(), Box<dyn Error>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install crypto provider");

    let url = Url::parse(
        &std::env::args()
            .nth(1)
            .unwrap_or_else(|| DEFAULT_URL.into()),
    )?;
    let event_loop = EventLoop::with_user_event().build()?;
    let mut app = App::Initial(Waker(event_loop.create_proxy()), url);
    Ok(event_loop.run_app(&mut app)?)
}

struct AppState {
    window: Window,
    servo: Servo,
    rendering_context: Rc<WindowRenderingContext>,
    webviews: RefCell<Vec<WebView>>,
}

impl WebViewDelegate for AppState {
    fn notify_new_frame_ready(&self, _: WebView) {
        self.window.request_redraw();
    }

    fn notify_page_title_changed(&self, _: WebView, title: Option<String>) {
        self.window
            .set_title(title.as_deref().unwrap_or("rever-servo-shell"));
    }

    // Traffic capture: every HTTP request the page makes passes through here.
    // Dropping `load` without calling `intercept` lets the request continue.
    fn load_web_resource(&self, _: WebView, load: WebResourceLoad) {
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
        println!("{{\"type\":\"console\",\"level\":\"{level:?}\",\"message\":{message:?}}}");
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        if status != LoadStatus::Complete {
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
    Initial(Waker, Url),
    Running(Rc<AppState>),
}

impl ApplicationHandler<WakerEvent> for App {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let Self::Initial(waker, url) = self else {
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

        let app_state = Rc::new(AppState {
            window,
            servo,
            rendering_context,
            webviews: Default::default(),
        });
        let webview = WebViewBuilder::new(&app_state.servo, app_state.rendering_context.clone())
            .url(url.clone())
            .hidpi_scale_factor(Scale::new(app_state.window.scale_factor() as f32))
            .delegate(app_state.clone())
            .build();
        app_state.webviews.borrow_mut().push(webview);
        *self = Self::Running(app_state);
    }

    fn user_event(&mut self, _: &winit::event_loop::ActiveEventLoop, _: WakerEvent) {
        if let Self::Running(state) = self {
            state.servo.spin_event_loop();
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
