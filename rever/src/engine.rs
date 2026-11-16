//! The Servo side of rever: one WebView per tab, each rendered off-screen. The UI asks
//! for frames as images and forwards input; everything here stays on the main thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use dpi::PhysicalSize;
use image::RgbaImage;
use servo::{
    ConsoleLogLevel, CreateNewWebViewRequest, DeviceIntRect, DeviceIntSize, EmbedderControl,
    EventLoopWaker, InputEvent, JSValue, RenderingContext, Servo, ServoBuilder,
    SoftwareRenderingContext, UserContentManager, UserScript, WebView, WebViewBuilder,
    WebViewDelegate,
};
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabId(pub u32);

/// What happened inside the engine since the last [`Engine::pump`].
pub enum EngineEvent {
    FrameReady(TabId),
    /// The page opened a window (`target="_blank"`, `window.open`): it is a new tab.
    Opened(TabId),
    Title(TabId, String),
    Url(TabId, String),
    Console {
        level: String,
        message: String,
    },
}

type Events = Rc<RefCell<Vec<EngineEvent>>>;
/// What the pages asked the interface to show: dialogs, file pickers, select lists.
type Controls = Rc<RefCell<Vec<(TabId, EmbedderControl)>>>;

struct Delegate {
    tab: TabId,
    events: Events,
    /// This tab's surface: a window it opens starts at the same size.
    context: Rc<SoftwareRenderingContext>,
    // Shared with the engine, which is borrowed while Servo calls the delegate.
    next_id: Rc<Cell<u32>>,
    opened: Rc<RefCell<Vec<Page>>>,
    controls: Controls,
}

impl Delegate {
    fn push(&self, event: EngineEvent) {
        self.events.borrow_mut().push(event);
    }
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, _: WebView) {
        self.push(EngineEvent::FrameReady(self.tab));
    }

    fn request_create_new(&self, parent: WebView, request: CreateNewWebViewRequest) {
        // Without a surface the request is dropped, which leaves the link dead.
        let Ok(context) = SoftwareRenderingContext::new(self.context.size()) else {
            return;
        };
        let context = Rc::new(context);
        let id = TabId(self.next_id.get());
        self.next_id.set(id.0 + 1);
        // The opener's init scripts (capture hooks, injections) apply to its windows too.
        let Some(scripts) = parent.user_content_manager() else {
            return;
        };
        let webview = request
            .builder(context.clone())
            .user_content_manager(scripts.clone())
            .hidpi_scale_factor(parent.hidpi_scale_factor())
            .delegate(Rc::new(Delegate {
                tab: id,
                events: self.events.clone(),
                context: context.clone(),
                next_id: self.next_id.clone(),
                opened: self.opened.clone(),
                controls: self.controls.clone(),
            }))
            .build();
        self.opened.borrow_mut().push(Page {
            id,
            webview,
            context,
            scripts,
        });
        self.push(EngineEvent::Opened(id));
    }

    fn show_embedder_control(&self, _: WebView, control: EmbedderControl) {
        // Dropping a control answers it with "dismissed", so it is kept for the app.
        self.controls.borrow_mut().push((self.tab, control));
    }

    fn notify_page_title_changed(&self, _: WebView, title: Option<String>) {
        self.push(EngineEvent::Title(self.tab, title.unwrap_or_default()));
    }

    fn notify_url_changed(&self, _: WebView, url: Url) {
        self.push(EngineEvent::Url(self.tab, url.into()));
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

struct Page {
    id: TabId,
    webview: WebView,
    context: Rc<SoftwareRenderingContext>,
    scripts: Rc<UserContentManager>,
}

pub struct Engine {
    servo: Servo,
    pages: Vec<Page>,
    events: Events,
    scale: f32,
    next_id: Rc<Cell<u32>>,
    /// Tabs the pages opened during the current pump.
    opened: Rc<RefCell<Vec<Page>>>,
    controls: Controls,
}

impl Engine {
    /// `wake` is called from Servo's threads whenever [`Engine::pump`] should run
    /// on the main thread.
    pub fn new(wake: impl Fn() + Send + Sync + 'static, scale: f32) -> Self {
        // Servo's TLS stack needs a process-wide crypto provider; GPUI may have installed one already.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let servo = ServoBuilder::default()
            // Off by default in Servo; React sites call it on load and fail without it.
            .preferences(servo::Preferences {
                dom_intersection_observer_enabled: true,
                ..Default::default()
            })
            .event_loop_waker(Box::new(Waker(Arc::new(wake))))
            .build();
        Self {
            servo,
            pages: Vec::new(),
            events: Events::default(),
            scale,
            next_id: Rc::new(Cell::new(1)),
            opened: Rc::default(),
            controls: Rc::default(),
        }
    }

    /// Open a tab. Sizes are device pixels.
    pub fn open(&mut self, url: Url, size: PhysicalSize<u32>) -> Result<TabId, String> {
        let context = Rc::new(
            SoftwareRenderingContext::new(size)
                .map_err(|error| format!("could not create a rendering surface: {error:?}"))?,
        );
        let id = TabId(self.next_id.get());
        self.next_id.set(id.0 + 1);
        let scripts = Rc::new(UserContentManager::new(&self.servo));
        let webview = WebViewBuilder::new(&self.servo, context.clone())
            .user_content_manager(scripts.clone())
            .url(url)
            .hidpi_scale_factor(euclid::Scale::new(self.scale))
            .delegate(Rc::new(Delegate {
                tab: id,
                events: self.events.clone(),
                context: context.clone(),
                next_id: self.next_id.clone(),
                opened: self.opened.clone(),
                controls: self.controls.clone(),
            }))
            .build();
        self.pages.push(Page {
            id,
            webview,
            context,
            scripts,
        });
        Ok(id)
    }

    pub fn close(&mut self, id: TabId) {
        self.pages.retain(|page| page.id != id);
    }

    fn page(&self, id: TabId) -> Option<&Page> {
        self.pages.iter().find(|page| page.id == id)
    }

    /// Let Servo process its queue; returns what changed.
    pub fn pump(&mut self) -> Vec<EngineEvent> {
        self.servo.spin_event_loop();
        self.pages.append(&mut self.opened.borrow_mut());
        std::mem::take(&mut *self.events.borrow_mut())
    }

    /// Paint the tab's page and read it back as RGBA.
    /// The dialogs and pickers the pages opened since the last call; each must be
    /// answered, or dropped to dismiss it.
    pub fn take_controls(&mut self) -> Vec<(TabId, EmbedderControl)> {
        std::mem::take(&mut *self.controls.borrow_mut())
    }

    pub fn frame(&self, id: TabId) -> Option<RgbaImage> {
        let page = self.page(id)?;
        page.webview.paint();
        let size = page.context.size();
        let area =
            DeviceIntRect::from_size(DeviceIntSize::new(size.width as i32, size.height as i32));
        let image = page.context.read_to_image(area);
        page.context.present();
        image
    }

    /// Device pixels per CSS pixel.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn size(&self, id: TabId) -> Option<PhysicalSize<u32>> {
        self.page(id).map(|page| page.context.size())
    }

    pub fn resize(&self, id: TabId, size: PhysicalSize<u32>) {
        if let Some(page) = self.page(id) {
            page.webview.resize(size);
        }
    }

    pub fn load(&self, id: TabId, url: Url) {
        if let Some(page) = self.page(id) {
            page.webview.load(url);
        }
    }

    pub fn back(&self, id: TabId) {
        if let Some(page) = self.page(id).filter(|page| page.webview.can_go_back()) {
            page.webview.go_back(1);
        }
    }

    pub fn forward(&self, id: TabId) {
        if let Some(page) = self.page(id).filter(|page| page.webview.can_go_forward()) {
            page.webview.go_forward(1);
        }
    }

    pub fn reload(&self, id: TabId) {
        if let Some(page) = self.page(id) {
            page.webview.reload();
        }
    }

    /// Evaluate JavaScript in the tab; `done` runs later, on the main thread.
    pub fn evaluate(
        &self,
        id: TabId,
        script: String,
        done: impl FnOnce(Result<JSValue, String>) + 'static,
    ) {
        match self.page(id) {
            Some(page) => page.webview.evaluate_javascript(script, move |result| {
                done(result.map_err(|error| format!("{error:?}")))
            }),
            None => done(Err("the tab is closed".to_owned())),
        }
    }

    /// Run `source` in every new document of the tab, before the page's own scripts.
    pub fn add_init_script(&self, id: TabId, source: String) {
        if let Some(page) = self.page(id) {
            page.scripts
                .add_script(Rc::new(UserScript::new(source, None)));
        }
    }

    pub fn input(&self, id: TabId, event: InputEvent) {
        if let Some(page) = self.page(id) {
            page.webview.notify_input_event(event);
        }
    }
}
