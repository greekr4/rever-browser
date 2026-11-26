//! The Servo side of rever: one WebView per tab, each rendered off-screen. The UI asks
//! for frames as images and forwards input; everything here stays on the main thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use dpi::PhysicalSize;
use image::RgbaImage;
use servo::{
    ConsoleLogLevel, CookieSource, CreateNewWebViewRequest, DeviceIntRect, DeviceIntSize,
    EmbedderControl, EventLoopWaker, InputEvent, JSValue, PrefValue, RenderingContext, Servo,
    ServoBuilder, ServoDelegate, SoftwareRenderingContext, Theme, UserContentManager, UserScript,
    WebDriverCommandMsg, WebDriverFrameId, WebDriverScriptCommand, WebResourceLoad, WebView,
    WebViewBuilder, WebViewDelegate,
};
use servo_base::generic_channel;
use servo_base::id::BrowsingContextId;
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
    /// The page's icon, as RGBA.
    Favicon(TabId, RgbaImage),
    Url(TabId, String),
    /// The answer to [`Engine::measure_memory`].
    Memory(MemoryReport),
    Console {
        level: String,
        message: String,
    },
}

/// The phone the TypeScript's mobile mode pretends to be (viewport.ts).
const MOBILE_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1";

/// A desktop identity for the engine's user agent.
///
/// Servo's own UA string names `Servo/0.8.0`, which antibot pages reject on
/// sight. A scan can present one of these Chrome UAs instead. Each entry is a
/// paired set: the UA's platform tokens and `platform` (what real Chrome
/// reports for `navigator.platform` with that UA) agree with each other, so
/// the UA string never contradicts itself. The table is the same data
/// obscura-browser ships (Apache-2.0).
// No caller yet: scan realism (a later milestone) pins one profile per
// session. Rotation between identities is itself a bot signal, so the
// caller decides; the table only guarantees each identity is self-consistent.
// allow (not expect): the tests read the table, so --all-targets would call
// the expectation unfulfilled there.
#[allow(dead_code)]
pub struct UserProfile {
    pub user_agent: &'static str,
    /// The platform the UA claims. Servo has no pref for `navigator.platform`
    /// and sends no UA client hints, so this is metadata today; a future
    /// servo patch can serve it instead of the real OS.
    pub platform: &'static str,
}

/// Stable per-OS identities: Chrome 143 to 146 on Windows and macOS. Rotation
/// between identities from one address is itself a bot signal, so callers pin
/// one profile for a session rather than cycling.
#[allow(dead_code)]
pub static USER_AGENT_PROFILES: &[UserProfile] = &[
    UserProfile {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36",
        platform: "Win32",
    },
    UserProfile {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36",
        platform: "Win32",
    },
    UserProfile {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36",
        platform: "Win32",
    },
    UserProfile {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        platform: "Win32",
    },
    UserProfile {
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36",
        platform: "MacIntel",
    },
    UserProfile {
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36",
        platform: "MacIntel",
    },
    UserProfile {
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36",
        platform: "MacIntel",
    },
    UserProfile {
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        platform: "MacIntel",
    },
];
/// The width of that phone in CSS pixels.
pub const MOBILE_WIDTH: f32 = 390.0;

type Events = Rc<RefCell<Vec<EngineEvent>>>;
/// Servo's memory report, cut down to the pages: the label of each set of documents
/// measured together (`url(a, b)`) and the bytes it holds.
pub type MemoryReport = Vec<(String, u64)>;
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

    fn notify_favicon_changed(&self, webview: WebView) {
        if let Some(icon) = webview.favicon().and_then(|image| favicon_rgba(&image)) {
            self.push(EngineEvent::Favicon(self.tab, icon));
        }
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
    /// Whether pages are told the color scheme is dark (`prefers-color-scheme`).
    dark: bool,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Servo's memory report, left here by its reply until the next pump.
    memory: Arc<Mutex<Option<MemoryReport>>>,
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
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let proxy = crate::profile_windows::engine_proxy().unwrap_or_default();
        let servo = ServoBuilder::default()
            // Where Servo reads and writes cookie_jar.json, so logins survive a restart.
            .opts(servo::Opts {
                config_dir: Some(profile_dir),
                ..Default::default()
            })
            // Off by default in Servo; React sites call it on load and fail without it.
            .preferences(servo::Preferences {
                dom_intersection_observer_enabled: true,
                // GitHub spreads `adoptedStyleSheets` of every root it styles; without the
                // property that throws, and its components never get their data.
                dom_adoptedstylesheet_enabled: true,
                // Variable fonts (one file for every weight, as next/font ships them):
                // without this every weight is drawn at the font's default.
                layout_css_font_variations_enabled: true,
                // Servo keeps the last 20 documents of every tab alive for an instant
                // "back", and as many ahead: a tab that had walked through news sites
                // held 16GB. None is kept: going back loads the page again.
                session_history_max_length: 0,
                // The debugger tools drive pages through Servo's debugging server.
                devtools_server_enabled: true,
                // On this machine only. Servo reports the port it was told, not the
                // one it got, so a free one is picked here first.
                devtools_server_listen_address: format!("127.0.0.1:{}", free_port()),
                // A proxy window's proxy: every request and WebSocket of this process.
                network_http_proxy_uri: proxy.clone(),
                network_https_proxy_uri: proxy,
                ..Default::default()
            })
            .event_loop_waker(Box::new(Waker(wake.clone())))
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
            dark: false,
            wake,
            memory: Arc::default(),
        }
    }

    /// Open a tab. Sizes are device pixels.
    pub fn open(&mut self, url: Url, size: PhysicalSize<u32>) -> Result<TabId, String> {
        let id = self.reserve();
        self.open_as(id, url, size)?;
        Ok(id)
    }

    /// An id for a tab whose page is opened later with [`Engine::open_as`].
    pub fn reserve(&self) -> TabId {
        let id = TabId(self.next_id.get());
        self.next_id.set(id.0 + 1);
        id
    }

    /// Open the page of the tab `id`: one that was reserved, or closed to sleep.
    pub fn open_as(&mut self, id: TabId, url: Url, size: PhysicalSize<u32>) -> Result<(), String> {
        let context = Rc::new(
            SoftwareRenderingContext::new(size)
                .map_err(|error| format!("could not create a rendering surface: {error:?}"))?,
        );
        let scripts = Rc::new(UserContentManager::new(&self.servo));
        // `window.__reverAi`: the tools call it to show what the agent does on the page.
        // Windows the page opens share this manager, so they get it too.
        scripts.add_script(Rc::new(UserScript::new(
            rever_inject::visualizer::VISUALIZER_INIT_SCRIPT.to_owned(),
            None,
        )));
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
        webview.notify_theme_change(page_theme(self.dark));
        self.pages.push(Page {
            id,
            webview,
            context,
            scripts,
        });
        Ok(())
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
        // A window a page opened has not been told the color scheme yet.
        for page in self.opened.borrow().iter() {
            page.webview.notify_theme_change(page_theme(self.dark));
        }
        self.pages.append(&mut self.opened.borrow_mut());
        let mut events = std::mem::take(&mut *self.events.borrow_mut());
        if let Some(report) = self.memory.lock().ok().and_then(|mut slot| slot.take()) {
            events.push(EngineEvent::Memory(report));
        }
        events
    }

    /// Ask Servo what each page holds; the answer comes as [`EngineEvent::Memory`].
    /// Every script thread walks its documents for it, so call it sparingly.
    pub fn measure_memory(&self) {
        use servo::profile_traits::mem::{MemoryReportResult, ReportKind};
        let (slot, wake) = (self.memory.clone(), self.wake.clone());
        let answered = servo_base::generic_channel::GenericCallback::new(
            move |result: Result<MemoryReportResult, _>| {
                let Ok(result) = result else { return };
                let report = result
                    .results
                    .into_iter()
                    .flat_map(|process| process.reports)
                    // "Non-explicit" sizes cut across the others: counting them counts twice.
                    .filter(|report| !matches!(report.kind, ReportKind::NonExplicitSize))
                    .filter_map(|report| {
                        let label = report.path.into_iter().next()?;
                        label
                            .starts_with("url(")
                            .then_some((label, report.size as u64))
                    })
                    .collect();
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(report);
                }
                wake();
            },
        );
        if let Ok(answered) = answered {
            self.servo.create_memory_report(answered);
        }
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

    /// Let a tab that is not on screen take in the frames it has built. WebRender
    /// hands every built frame to the tab's renderer, which takes them in only when it
    /// paints: a tab that was never painted kept each frame it ever built (128KB and
    /// more apiece), and a page animating in the background grew without end.
    pub fn settle(&self, id: TabId) {
        if let Some(page) = self.page(id) {
            page.webview.paint();
            page.context.present();
        }
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
        self.apply_user_agent(&user_agent);
    }

    /// Switch the whole engine to a desktop identity from
    /// [`USER_AGENT_PROFILES`]. Like the mobile toggle, the user agent is one
    /// setting for the whole engine, so it holds for every tab.
    #[allow(dead_code)]
    pub fn set_user_agent_profile(&self, profile: &'static UserProfile) {
        self.apply_user_agent(profile.user_agent);
    }

    fn apply_user_agent(&self, user_agent: &str) {
        self.servo
            .set_preference("user_agent", PrefValue::Str(user_agent.to_owned()));
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

    /// The cursor the page asks for under the pointer (`cursor: pointer`, text, ...).
    pub fn cursor(&self, id: TabId) -> servo::Cursor {
        self.page(id)
            .map(|page| page.webview.cursor())
            .unwrap_or_default()
    }

    /// Chrome's zoom steps: `None` resets to 100%, otherwise one step in or out.
    pub fn zoom(&self, id: TabId, step_in: Option<bool>) {
        let Some(page) = self.page(id) else { return };
        let current = page.webview.page_zoom();
        let next = match step_in {
            None => Some(1.0),
            Some(true) => ZOOM_STEPS.iter().copied().find(|z| *z > current + 0.001),
            Some(false) => ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|z| *z < current - 0.001),
        };
        if let Some(zoom) = next {
            page.webview.set_page_zoom(zoom);
        }
    }

    /// Tell every page, and the ones opened later, whether the color scheme is dark, so
    /// `prefers-color-scheme` follows the app's theme as it follows the OS in Chrome.
    pub fn set_dark(&mut self, dark: bool) {
        if self.dark == dark {
            return;
        }
        self.dark = dark;
        for page in &self.pages {
            page.webview.notify_theme_change(page_theme(dark));
        }
    }

    /// The address of the link under the pointer, which the page reports for a status bar.
    pub fn hovered_link(&self, id: TabId) -> Option<String> {
        self.page(id)?.webview.status_text()
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

    /// Send a command to the script thread of one of the tab's documents: the tab's own,
    /// or the one in `frame`.
    fn frame_command(
        &self,
        id: TabId,
        frame: Option<BrowsingContextId>,
        command: WebDriverScriptCommand,
    ) -> Result<(), String> {
        let page = self.page(id).ok_or("the tab is closed")?;
        let context = frame.unwrap_or_else(|| page.webview.id().into());
        self.servo
            .execute_webdriver_command(WebDriverCommandMsg::ScriptCommand(context, command));
        Ok(())
    }

    /// Evaluate JavaScript in a frame's document, whatever its origin (`None`: the
    /// tab's own). A promise is awaited. `done` runs on a thread of its own: the
    /// answer does not come through `pump`, so something has to wait for it.
    pub fn evaluate_in(
        &self,
        id: TabId,
        frame: Option<BrowsingContextId>,
        script: String,
        done: impl FnOnce(Result<JSValue, String>) + Send + 'static,
    ) {
        let Some((sender, answered)) = generic_channel::channel() else {
            return done(Err("no channel to the page".to_owned()));
        };
        let command = WebDriverScriptCommand::ExecuteScriptWithCallback(script, sender);
        if let Err(error) = self.frame_command(id, frame, command) {
            return done(Err(error));
        }
        std::thread::spawn(move || {
            done(match answered.recv() {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(format!("{error:?}")),
                Err(_) => Err("the frame is gone".to_owned()),
            })
        });
    }

    /// The frame an `<iframe>` element holds. `element` is what `evaluate_in` answered
    /// for the element, in the document `parent` names.
    pub fn frame_of(
        &self,
        id: TabId,
        parent: Option<BrowsingContextId>,
        element: String,
        done: impl FnOnce(Result<BrowsingContextId, String>) + Send + 'static,
    ) {
        let Some((sender, answered)) = generic_channel::channel() else {
            return done(Err("no channel to the page".to_owned()));
        };
        let command = WebDriverScriptCommand::GetBrowsingContextId(
            WebDriverFrameId::Element(element),
            sender,
        );
        if let Err(error) = self.frame_command(id, parent, command) {
            return done(Err(error));
        }
        // The script thread panics when nobody is left to hear this answer, so the
        // wait has no time limit.
        std::thread::spawn(move || {
            done(match answered.recv() {
                Ok(Ok(frame)) => Ok(frame),
                Ok(Err(error)) => Err(format!("the element holds no frame: {error:?}")),
                Err(_) => Err("the frame is gone".to_owned()),
            })
        });
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

/// Chrome's zoom levels.
const ZOOM_STEPS: [f32; 15] = [
    0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0,
];

/// Servo hands the icon in one of several layouts; the tab strip wants RGBA.
fn page_theme(dark: bool) -> Theme {
    if dark { Theme::Dark } else { Theme::Light }
}

fn favicon_rgba(image: &servo::Image) -> Option<RgbaImage> {
    use servo::PixelFormat;
    let data = image.data();
    let rgba: Vec<u8> = match image.format {
        PixelFormat::RGBA8 => data.to_vec(),
        PixelFormat::BGRA8 => data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[2], p[1], p[0], p[3]])
            .collect(),
        PixelFormat::RGB8 => data
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        PixelFormat::KA8 => data
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        PixelFormat::K8 => data.iter().flat_map(|k| [*k, *k, *k, 255]).collect(),
    };
    RgbaImage::from_raw(image.width, image.height, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 프로필의_ua와_플랫폼_토큰은_서로_맞는다() {
        // 테이블이 존재하는 이유가 이 정합성이다: UA 문자열 안의 플랫폼 토큰이
        // platform 필드(실제 크롬이 같은 UA에서 내는 navigator.platform)와 어긋나면
        // 지문 불일치가 탐지의 단서가 된다.
        for profile in USER_AGENT_PROFILES {
            let expected = match profile.platform {
                "Win32" => "Windows NT 10.0; Win64",
                "MacIntel" => "Macintosh; Intel Mac OS X",
                other => panic!("unknown platform token: {other}"),
            };
            assert!(
                profile.user_agent.contains(expected),
                "{} does not match its platform {}",
                profile.user_agent,
                profile.platform
            );
        }
    }

    #[test]
    fn 프로필의_크롬_버전과_스킨은_정합한다() {
        // Chrome 버전 토큰은 하나뿐이어야 하고 Safari 토큰은 항상 마지막에 온다:
        // 실제 크롬 UA의 고정 형태다.
        for profile in USER_AGENT_PROFILES {
            let chrome = profile.user_agent.matches("Chrome/").count();
            assert_eq!(chrome, 1, "{}", profile.user_agent);
            assert!(
                profile.user_agent.ends_with("Safari/537.36"),
                "{}",
                profile.user_agent
            );
        }
    }
}
