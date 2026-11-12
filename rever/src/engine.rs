//! The Servo side of rever: one WebView rendered off-screen. The UI asks for
//! frames as images and forwards input; everything here stays on the main thread.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use dpi::PhysicalSize;
use image::RgbaImage;
use servo::{
    ConsoleLogLevel, DeviceIntRect, DeviceIntSize, EventLoopWaker, InputEvent, RenderingContext,
    Servo, ServoBuilder, SoftwareRenderingContext, WebView, WebViewBuilder, WebViewDelegate,
};
use url::Url;

/// What happened inside the engine since the last [`Engine::pump`].
pub enum EngineEvent {
    FrameReady,
    Title(String),
    Url(String),
    Console { level: String, message: String },
}

#[derive(Default)]
struct Delegate {
    events: RefCell<Vec<EngineEvent>>,
}

impl Delegate {
    fn push(&self, event: EngineEvent) {
        self.events.borrow_mut().push(event);
    }
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, _: WebView) {
        self.push(EngineEvent::FrameReady);
    }

    fn notify_page_title_changed(&self, _: WebView, title: Option<String>) {
        self.push(EngineEvent::Title(title.unwrap_or_default()));
    }

    fn notify_url_changed(&self, _: WebView, url: Url) {
        self.push(EngineEvent::Url(url.into()));
    }

    fn show_console_message(&self, _: WebView, level: ConsoleLogLevel, message: String) {
        self.push(EngineEvent::Console {
            level: format!("{level:?}").to_lowercase(),
            message,
        });
    }
}

#[derive(Clone)]
struct Waker(Arc<dyn Fn() + Send + Sync>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        (self.0)();
    }
}

pub struct Engine {
    servo: Servo,
    webview: WebView,
    context: Rc<SoftwareRenderingContext>,
    delegate: Rc<Delegate>,
}

impl Engine {
    /// `wake` is called from Servo's threads whenever [`Engine::pump`] should run
    /// on the main thread. Sizes are device pixels.
    pub fn new(
        wake: impl Fn() + Send + Sync + 'static,
        size: PhysicalSize<u32>,
        scale: f32,
        url: Url,
    ) -> Result<Self, String> {
        // Servo's TLS stack needs a process-wide crypto provider; GPUI may have installed one already.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let context = Rc::new(
            SoftwareRenderingContext::new(size)
                .map_err(|error| format!("could not create a rendering surface: {error:?}"))?,
        );
        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(Waker(Arc::new(wake))))
            .build();
        let delegate = Rc::new(Delegate::default());
        let webview = WebViewBuilder::new(&servo, context.clone())
            .url(url)
            .hidpi_scale_factor(euclid::Scale::new(scale))
            .delegate(delegate.clone())
            .build();
        Ok(Self {
            servo,
            webview,
            context,
            delegate,
        })
    }

    /// Let Servo process its queue; returns what changed.
    pub fn pump(&self) -> Vec<EngineEvent> {
        self.servo.spin_event_loop();
        std::mem::take(&mut *self.delegate.events.borrow_mut())
    }

    /// Paint the page and read it back as RGBA.
    pub fn frame(&self) -> Option<RgbaImage> {
        self.webview.paint();
        let size = self.context.size();
        let area =
            DeviceIntRect::from_size(DeviceIntSize::new(size.width as i32, size.height as i32));
        let image = self.context.read_to_image(area);
        self.context.present();
        image
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.context.size()
    }

    pub fn resize(&self, size: PhysicalSize<u32>) {
        self.webview.resize(size);
    }

    pub fn load(&self, url: Url) {
        self.webview.load(url);
    }

    pub fn input(&self, event: InputEvent) {
        self.webview.notify_input_event(event);
    }
}
