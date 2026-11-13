//! Opens this crate's views alone with fixture data. See rever_ui_kit::preview.
//!
//! A toolbar holds the proxy chip and the profile button; the two modals lie over the
//! window. `REVER_PREVIEW_VIEW` chooses what is open:
//!
//! - `onboarding` (default): the first-run provider setup
//! - `import`: the browser import modal (onboarding already done)
//! - `profile`: the profile menu, with Chrome's profiles expanded
//! - `proxy`: the proxy popover on a plain tab (it will open a new proxied tab)
//! - `proxy-on`: the popover on a tab that already runs through a proxy
//!
//! The line in the page area names the last thing a view asked the host to do.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{App, Context, Entity, EntityId, Task, Window, div, prelude::*, px};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use rever_ui_setup::browser_import_modal::BrowserImportModal;
use rever_ui_setup::host::{
    AgentHealth, AgentHealthStatus, BrowserId, BrowserImportOptions, BrowserImportResult,
    BrowserInfo, BrowserProfile, BrowserProfileInfo, KeyProvider, ProfileKind, SetupHost,
};
use rever_ui_setup::onboarding_modal::OnboardingModal;
use rever_ui_setup::profile_menu::ProfileMenu;
use rever_ui_setup::profiles::ProfilesStore;
use rever_ui_setup::proxy_button::ProxyButton;
use rever_ui_setup::ui::{Look, LookFn};
use rever_ui_state::agent_choice::AgentChoiceStore;
use rever_ui_state::constants::AcpAgentId;
use rever_ui_state::navigation_request::NavigationRequestStore;
use rever_ui_state::tabs::{AddTabOptions, ProxyConfig, ProxyScheme, TabsStore};
use rever_ui_state::{MemoryStorage, Storage};

const WIDTH: f32 = 1000.0;
const HEIGHT: f32 = 720.0;
const TOOLBAR: f32 = 46.0;

/// Stands in for the main process and the renderer stores: answers after a short wait,
/// as IPC does, and keeps the tabs, the profiles and the agent choice in memory.
struct PreviewHost {
    storage: Rc<MemoryStorage>,
    choice: RefCell<AgentChoiceStore>,
    tabs: RefCell<TabsStore>,
    navigation: RefCell<NavigationRequestStore>,
    profiles: RefCell<Vec<BrowserProfile>>,
    /// Providers a key was saved for: the next probe finds them ready.
    keyed: RefCell<Vec<KeyProvider>>,
    last: RefCell<String>,
    root: Cell<Option<EntityId>>,
}

impl PreviewHost {
    fn say(&self, text: String, cx: &mut App) {
        *self.last.borrow_mut() = text;
        if let Some(root) = self.root.get() {
            cx.notify(root);
        }
    }

    /// Resolve with `value` after a moment.
    fn later<T: 'static>(&self, value: T, cx: &mut App) -> Task<T> {
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            value
        })
    }
}

fn health(id: &str, status: AgentHealthStatus) -> AgentHealth {
    AgentHealth {
        id: id.to_owned(),
        status,
        resolved_path: None,
        detail: None,
        auto: false,
        plan: None,
    }
}

fn browsers() -> Vec<BrowserInfo> {
    let profile = |id: &str, name: &str| BrowserProfileInfo {
        id: id.to_owned(),
        name: name.to_owned(),
    };
    vec![
        BrowserInfo {
            id: BrowserId::Chrome,
            name: "Chrome".to_owned(),
            profiles: vec![profile("Default", "Personal"), profile("Profile 1", "Work")],
        },
        BrowserInfo {
            id: BrowserId::Arc,
            name: "Arc".to_owned(),
            profiles: vec![profile("Default", "Default")],
        },
        BrowserInfo {
            id: BrowserId::Firefox,
            name: "Firefox".to_owned(),
            profiles: vec![profile("abcd.default-release", "default-release")],
        },
    ]
}

impl SetupHost for PreviewHost {
    fn probe_agents(&self, cx: &mut App) -> Task<Vec<AgentHealth>> {
        let keyed = self.keyed.borrow();
        let api = |id: &str, provider: KeyProvider, otherwise: AgentHealth| {
            if keyed.contains(&provider) {
                AgentHealth {
                    plan: Some("api-key".to_owned()),
                    ..health(id, AgentHealthStatus::Ready)
                }
            } else {
                otherwise
            }
        };
        let results = vec![
            AgentHealth {
                resolved_path: Some("/usr/local/bin/claude".to_owned()),
                auto: true,
                plan: Some("max".to_owned()),
                ..health("claude-code", AgentHealthStatus::Ready)
            },
            health("codex", AgentHealthStatus::NotInstalled),
            api(
                "anthropic",
                KeyProvider::Anthropic,
                health("anthropic", AgentHealthStatus::NeedsKey),
            ),
            api(
                "openai",
                KeyProvider::Openai,
                AgentHealth {
                    detail: Some("401 Incorrect API key provided".to_owned()),
                    ..health("openai", AgentHealthStatus::AuthFailed)
                },
            ),
            api(
                "grok",
                KeyProvider::Grok,
                health("grok", AgentHealthStatus::NeedsLogin),
            ),
            api(
                "glm",
                KeyProvider::Glm,
                health("glm", AgentHealthStatus::NeedsKey),
            ),
        ];
        drop(keyed);
        self.later(results, cx)
    }

    fn set_api_key(
        &self,
        provider: KeyProvider,
        key: String,
        cx: &mut App,
    ) -> Task<Result<(), String>> {
        self.keyed.borrow_mut().push(provider);
        // The key itself is not echoed, here or anywhere.
        self.say(
            format!(
                "settings.setApiKey({provider:?}, {} characters)",
                key.chars().count()
            ),
            cx,
        );
        self.later(Ok(()), cx)
    }

    fn browsers(&self, cx: &mut App) -> Task<Vec<BrowserInfo>> {
        self.later(browsers(), cx)
    }

    fn browser_import(
        &self,
        opts: BrowserImportOptions,
        cx: &mut App,
    ) -> Task<Result<BrowserImportResult, String>> {
        self.say(format!("storage.browserImport({opts:?})"), cx);
        self.later(
            Ok(BrowserImportResult {
                ok: true,
                imported: 1_482,
                skipped: 12,
                undecryptable: 37,
                total: 1_531,
                error: None,
            }),
            cx,
        )
    }

    fn list_profiles(&self, cx: &mut App) -> Task<Result<Vec<BrowserProfile>, String>> {
        self.later(Ok(self.profiles.borrow().clone()), cx)
    }

    fn create_profile(
        &self,
        name: String,
        kind: ProfileKind,
        source: Option<String>,
        cx: &mut App,
    ) -> Task<Result<BrowserProfile, String>> {
        let number = self.profiles.borrow().len();
        let profile = BrowserProfile {
            id: format!("p{number}"),
            name: if name.is_empty() {
                format!("Profile {number}")
            } else {
                name
            },
            kind,
            partition: match kind {
                ProfileKind::Persistent => format!("persist:rever-profile-p{number}"),
                ProfileKind::Incognito => format!("rever-incognito-p{number}"),
            },
            source,
        };
        self.profiles.borrow_mut().push(profile.clone());
        self.say(format!("profiles.create({:?}, {kind:?})", profile.name), cx);
        self.later(Ok(profile), cx)
    }

    fn delete_profile(&self, id: String, cx: &mut App) -> Task<Result<(), String>> {
        self.profiles
            .borrow_mut()
            .retain(|profile| profile.id != id);
        self.say(format!("profiles.delete({id:?})"), cx);
        self.later(Ok(()), cx)
    }

    fn register_tab_partition(
        &self,
        tab_id: String,
        partition: String,
        cx: &mut App,
    ) -> Task<Result<(), String>> {
        self.say(
            format!("profiles.registerTabPartition({tab_id:?}, {partition:?})"),
            cx,
        );
        self.later(Ok(()), cx)
    }

    fn set_proxy(
        &self,
        tab_id: String,
        config: Option<ProxyConfig>,
        partition: Option<String>,
        cx: &mut App,
    ) -> Task<Result<(), String>> {
        // The password stays out of the log line.
        let config = config.map(|c| format!("{:?}://{}:{}", c.scheme, c.host, c.port));
        self.say(
            format!("proxy.set({tab_id:?}, {config:?}, {partition:?})"),
            cx,
        );
        self.later(Ok(()), cx)
    }

    fn set_active_tab(&self, tab_id: String, cx: &mut App) -> Task<Result<(), String>> {
        self.say(format!("proxy.setActiveTab({tab_id:?})"), cx);
        self.later(Ok(()), cx)
    }

    fn onboarded(&self, _: &App) -> bool {
        self.choice.borrow().onboarded()
    }

    fn pick_agent(&self, id: AcpAgentId, path: Option<String>, cx: &mut App) {
        let _ = self
            .choice
            .borrow_mut()
            .pick(self.storage.as_ref(), id, path.clone());
        self.say(format!("agentChoice.pick({:?}, {path:?})", id.as_str()), cx);
    }

    fn skip_onboarding(&self, cx: &mut App) {
        let _ = self.choice.borrow_mut().skip(self.storage.as_ref());
        self.say("agentChoice.skip()".to_owned(), cx);
    }

    fn add_tab(&self, url: &str, opts: AddTabOptions, cx: &mut App) -> String {
        let id = self.tabs.borrow_mut().add_tab(url, opts);
        self.say(format!("tabs.addTab({url:?}) → {id}"), cx);
        id
    }

    fn close_tab(&self, id: &str, cx: &mut App) {
        self.tabs.borrow_mut().close_tab(id);
        self.say(format!("tabs.closeTab({id:?})"), cx);
    }

    fn set_tab_proxy(&self, id: &str, proxy: Option<ProxyConfig>, cx: &mut App) {
        self.tabs.borrow_mut().set_tab_proxy(id, proxy);
        self.say(format!("tabs.setTabProxy({id:?})"), cx);
    }

    fn request_navigation(&self, url: &str, cx: &mut App) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as u64);
        self.navigation.borrow_mut().request(url, now);
        self.say(format!("navigation.request({url:?})"), cx);
    }
}

struct Preview {
    look: Look,
    host: Rc<PreviewHost>,
    proxy: Entity<ProxyButton>,
    profile: Entity<ProfileMenu>,
    onboarding: Entity<OnboardingModal>,
    import: Entity<BrowserImportModal>,
}

impl Preview {
    fn new(cx: &mut Context<Self>) -> Self {
        let view = std::env::var("REVER_PREVIEW_VIEW").unwrap_or_default();
        let look = Look {
            theme_mode: ThemeMode::from_env(),
            lang: Lang::detect(),
        };
        let look_fn: LookFn = Rc::new(move |_| look);

        let storage = Rc::new(MemoryStorage::new());
        let mut choice = AgentChoiceStore::load(storage.as_ref());
        if !matches!(view.as_str(), "" | "onboarding") {
            let _ = choice.skip(storage.as_ref());
        }
        if view != "import" {
            // The import offer is a first-run thing; keep it out of the other previews.
            let _ = storage.set("rev:browser-import-offered", "1");
        }
        let mut tabs = TabsStore::new();
        if view == "proxy-on" {
            tabs.add_tab(
                "https://example.com/account",
                AddTabOptions {
                    proxy: Some(ProxyConfig {
                        enabled: true,
                        scheme: ProxyScheme::Socks5,
                        host: "10.8.0.2".to_owned(),
                        port: 1080,
                        username: Some("tk".to_owned()),
                        password: Some("hunter2".to_owned()),
                    }),
                    partition: Some("rever-proxy-m3k9x1ab".to_owned()),
                    ..AddTabOptions::default()
                },
            );
        }
        let profile = |id: &str, name: &str, kind, source: Option<&str>| BrowserProfile {
            id: id.to_owned(),
            name: name.to_owned(),
            kind,
            partition: format!("persist:rever-profile-{id}"),
            source: source.map(str::to_owned),
        };
        let host = Rc::new(PreviewHost {
            storage: storage.clone(),
            choice: RefCell::new(choice),
            tabs: RefCell::new(tabs),
            navigation: RefCell::default(),
            profiles: RefCell::new(vec![
                profile("default", "Default", ProfileKind::Persistent, None),
                profile("p1", "Work", ProfileKind::Persistent, Some("Chrome")),
                profile("p2", "Scratch", ProfileKind::Incognito, None),
            ]),
            keyed: RefCell::default(),
            last: RefCell::new("(nothing yet)".to_owned()),
            root: Cell::new(Some(cx.entity_id())),
        });
        let setup: Rc<dyn SetupHost> = host.clone();

        let proxy = cx.new(|cx| {
            let tabs = host.clone();
            let applied = host.clone();
            ProxyButton::new(
                setup.clone(),
                move |_| {
                    let tabs = tabs.tabs.borrow();
                    tabs.tabs()
                        .iter()
                        .find(|tab| tab.id == tabs.active_id())
                        .cloned()
                },
                move |cx| applied.say("onApplied(): reload the tab".to_owned(), cx),
                look_fn.clone(),
                cx,
            )
        });
        let store = cx.new(|_| ProfilesStore::new(setup.clone()));
        let profile = cx.new(|cx| ProfileMenu::new(store, setup.clone(), look_fn.clone(), cx));
        let onboarding = cx.new(|cx| OnboardingModal::new(setup.clone(), look_fn.clone(), cx));
        let import = cx.new(|_| BrowserImportModal::new(setup.clone(), storage, look_fn.clone()));

        match view.as_str() {
            "proxy" | "proxy-on" => proxy.update(cx, |proxy, cx| proxy.set_open(true, cx)),
            "profile" => profile.update(cx, |menu, cx| {
                menu.set_open(true, cx);
                menu.toggle_browser(BrowserId::Chrome, cx);
            }),
            "import" => import.update(cx, |import, cx| import.open(cx)),
            _ => {}
        }

        Self {
            look,
            host,
            proxy,
            profile,
            onboarding,
            import,
        }
    }
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::resolve(self.look.theme_mode, window.appearance());
        let tabs = self.host.tabs.borrow();
        let open_tabs = tabs
            .tabs()
            .iter()
            .map(|tab| {
                let mark = if tab.id == tabs.active_id() {
                    "●"
                } else {
                    "○"
                };
                format!("{mark} {} {}", tab.id, tab.url)
            })
            .collect::<Vec<_>>();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(".SystemUIFont")
            .text_size(px(13.0))
            .child(
                div()
                    .h(px(TOOLBAR))
                    .flex_none()
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(4.0))
                    .bg(theme.chrome_toolbar)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(self.proxy.clone())
                    .child(self.profile.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .p(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .child(format!("last host call: {}", self.host.last.borrow()))
                    .children(open_tabs),
            )
            // Last, as App.tsx mounts them: over everything else.
            .child(self.onboarding.clone())
            .child(self.import.clone())
    }
}

fn main() {
    rever_ui_kit::preview::run(WIDTH, HEIGHT, |_, cx| cx.new(Preview::new));
}
