//! The Servo side of rever: one WebView per tab, each rendered off-screen. The UI asks
//! for frames as images and forwards input; everything here stays on the main thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use dpi::PhysicalSize;
use image::RgbaImage;
use servo::{
    ConsoleLogLevel, CookieSource, CreateNewWebViewRequest, DeviceIntRect, DeviceIntSize,
    EmbedderControl, EventLoopWaker, InputEvent, JSValue, PrefValue, RenderingContext, Servo,
    ServoBuilder, ServoDelegate, SoftwareRenderingContext, UserContentManager, UserScript,
    WebResourceLoad, WebView, WebViewBuilder, WebViewDelegate,
};
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabId(pub u32);

/// What happened inside the engine since the last [`Engine::pump`].
pub enum EngineEvent {
    FrameReady(TabId),
    /// The page opened a window (`target="_blank"`, `window.open`): it is a new tab.
    Opened(TabId),
    /// The page closed itself with `window.close()`.
    Closed(TabId),
    Title(TabId, String),
    Url(TabId, String),
    Console {
        level: String,
        message: String,
    },
}

/// The phone the TypeScript's mobile mode pretends to be (viewport.ts).
const MOBILE_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1";
/// The width of that phone in CSS pixels.
pub const MOBILE_WIDTH: f32 = 390.0;

type Events = Rc<RefCell<Vec<EngineEvent>>>;
/// What the pages asked the interface to show: dialogs, file pickers, select lists.
type Controls = Rc<RefCell<Vec<(TabId, EmbedderControl)>>>;
/// Decides each HTTP request before it is sent; dropping the load sends it.
type LoadHook = Rc<RefCell<Option<Box<dyn FnMut(WebResourceLoad)>>>>;

struct Delegate {
    tab: TabId,
    events: Events,
    /// This tab's surface: a window it opens starts at the same size.
    context: Rc<SoftwareRenderingContext>,
    // Shared with the engine, which is borrowed while Servo calls the delegate.
    next_id: Rc<Cell<u32>>,
    opened: Rc<RefCell<Vec<Page>>>,
    controls: Controls,
    loads: LoadHook,
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
                loads: self.loads.clone(),
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

    fn load_web_resource(&self, _: WebView, load: WebResourceLoad) {
        if let Some(hook) = self.loads.borrow_mut().as_mut() {
            hook(load);
        }
    }

    fn notify_closed(&self, _: WebView) {
        self.push(EngineEvent::Closed(self.tab));
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

/// Where Servo's own debugging server listens and the token that admits a client:
/// the app is its only client, anyone else is refused by default.
type Devtools = Rc<RefCell<Option<(u16, String)>>>;

struct AppDelegate(Devtools);

impl ServoDelegate for AppDelegate {
    fn notify_devtools_server_started(&self, port: u16, token: String) {
        *self.0.borrow_mut() = Some((port, token));
    }
}

/// A port nothing listens on right now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map_or(0, |address| address.port())
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
    devtools: Devtools,
    servo: Servo,
    pages: Vec<Page>,
    events: Events,
    scale: f32,
    next_id: Rc<Cell<u32>>,
    /// Tabs the pages opened during the current pump.
    opened: Rc<RefCell<Vec<Page>>>,
    controls: Controls,
    loads: LoadHook,
    /// What the engine called itself at start, to go back to from mobile mode.
    desktop_user_agent: String,
    /// Init scripts by the id they were handed out under: Servo removes one only
    /// by the handle it was added with.
    init_scripts: Vec<(String, TabId, Rc<UserScript>)>,
    next_script: u32,
}

impl Engine {
    /// `wake` is called from Servo's threads whenever [`Engine::pump`] should run
    /// on the main thread.
    /// `profile_dir` is where Servo keeps its persistent state (cookies, HSTS,
    /// auth cache) between runs.
    pub fn new(
        wake: impl Fn() + Send + Sync + 'static,
        scale: f32,
        profile_dir: std::path::PathBuf,
    ) -> Self {
        // Servo's TLS stack needs a process-wide crypto provider; GPUI may have installed one already.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let _ = std::fs::create_dir_all(&profile_dir);
        let servo = ServoBuilder::default()
            // Where Servo reads and writes cookie_jar.json, so logins survive a restart.
            .opts(servo::Opts {
                config_dir: Some(profile_dir),
                ..Default::default()
            })
            // Off by default in Servo; React sites call it on load and fail without it.
            .preferences(servo::Preferences {
                dom_intersection_observer_enabled: true,
                // The debugger tools drive pages through Servo's debugging server.
                devtools_server_enabled: true,
                // On this machine only. Servo reports the port it was told, not the
                // one it got, so a free one is picked here first.
                devtools_server_listen_address: format!("127.0.0.1:{}", free_port()),
                ..Default::default()
            })
            .event_loop_waker(Box::new(Waker(Arc::new(wake))))
            .build();
        let devtools = Devtools::default();
        servo.set_delegate(Rc::new(AppDelegate(devtools.clone())));
        Self {
            devtools,
            servo,
            pages: Vec::new(),
            events: Events::default(),
            scale,
            next_id: Rc::new(Cell::new(1)),
            opened: Rc::default(),
            controls: Rc::default(),
            loads: Rc::default(),
            desktop_user_agent: servo::prefs::get().user_agent.clone(),
            init_scripts: Vec::new(),
            next_script: 1,
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
                loads: self.loads.clone(),
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

    /// The tab's WebView id hashed to a u64 — the same key the network tap
    /// stamps onto captured requests (same fixed-seed hash of the same
    /// WebViewId), so a tab index resolves to its captured requests.
    pub fn webview_u64(&self, id: TabId) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let wid = self.page(id)?.webview.id();
        let mut hasher = std::hash::DefaultHasher::new();
        wid.hash(&mut hasher);
        Some(hasher.finish())
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
    /// Have `hook` decide every HTTP request of every tab before it is sent.
    pub fn on_resource_load(&self, hook: impl FnMut(WebResourceLoad) + 'static) {
        *self.loads.borrow_mut() = Some(Box::new(hook));
    }

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

    /// The cookies sent to `url`, HTTP-only ones included.
    pub fn cookies(&self, url: Url) -> Vec<cookie::Cookie<'static>> {
        self.servo
            .site_data_manager()
            .cookies_for_url(url, CookieSource::HTTP)
    }

    /// Store a cookie as if `url` had set it. One that is already expired removes
    /// the cookie of that name, which is how a cookie is deleted.
    pub fn set_cookie(&self, url: Url, cookie: cookie::Cookie<'static>) {
        self.servo
            .site_data_manager()
            .set_cookie_for_url(url, cookie, None);
    }

    /// Identify as a phone, or as the desktop browser again. The user agent is one
    /// setting for the whole engine, so it holds for every tab.
    pub fn set_mobile_user_agent(&self, mobile: bool) {
        let user_agent = if mobile {
            MOBILE_USER_AGENT.to_owned()
        } else {
            self.desktop_user_agent.clone()
        };
        self.servo
            .set_preference("user_agent", PrefValue::Str(user_agent));
    }

    /// The port of Servo's debugging server and the token that admits a client;
    /// `None` until the server has started.
    pub fn devtools(&self) -> Option<(u16, String)> {
        self.devtools.borrow().clone()
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
    /// Returns the id to remove the script with; `None` when the tab is gone.
    pub fn add_init_script(&mut self, id: TabId, source: String) -> Option<String> {
        let script = Rc::new(UserScript::new(source, None));
        self.page(id)?.scripts.add_script(script.clone());
        let script_id = self.next_script.to_string();
        self.next_script += 1;
        self.init_scripts.push((script_id.clone(), id, script));
        Some(script_id)
    }

    /// False when no script has this id.
    pub fn remove_init_script(&mut self, script_id: &str) -> bool {
        let Some(at) = self
            .init_scripts
            .iter()
            .position(|(id, ..)| id == script_id)
        else {
            return false;
        };
        let (_, tab, script) = self.init_scripts.remove(at);
        if let Some(page) = self.page(tab) {
            page.scripts.remove_script(script);
        }
        true
    }

    pub fn input(&self, id: TabId, event: InputEvent) {
        if let Some(page) = self.page(id) {
            page.webview.notify_input_event(event);
        }
    }
}
