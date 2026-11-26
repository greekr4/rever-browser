//! What the first-run views ask of the app: which agents can run here, where an API
//! key goes, Chrome's cookies, and the agent the user picks.
//!
//! A profile or a proxy is a window of its own ([`crate::profile_windows`]): the engine
//! has one cookie jar and one proxy per process, so neither can differ between tabs.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Task, WeakEntity, prelude::*};
use rever_tools_api::tool::Browser as PageBrowser;
use rever_ui_setup::host::{
    AgentHealth, BrowserId, BrowserImportOptions, BrowserImportResult, BrowserInfo, BrowserProfile,
    BrowserProfileInfo, KeyProvider, ProfileKind, SetupHost,
};
use rever_ui_setup::profiles::DEFAULT_PROFILE_ID;
use rever_ui_state::Storage;
use rever_ui_state::agent_choice::{AgentChoiceStore, has_onboarded};
use rever_ui_state::constants::AcpAgentId;
use rever_ui_state::tabs::{AddTabOptions, ProxyConfig};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::api_keys::{ApiProvider, set_api_key};
use crate::app::{AGENT_STORAGE_KEY, Browser, data_dir, now_ms};
use crate::profile_windows::{
    Profiles, WindowSpec, ephemeral_folder, leave_cookies, open_window, proxy_uri,
    restart_with_proxy, this_window,
};
use crate::storage_host::{chrome_installed, import_chrome, read_chrome};

/// Whether this user is past the first run: an agent was picked or the guide skipped,
/// here or before the guide existed. A self-check run never shows it.
fn set_up_before(storage: &dyn Storage, self_check: bool) -> bool {
    // A profile or proxy window starts on an empty folder every time it is new.
    self_check
        || this_window().is_some()
        || has_onboarded(storage)
        || storage.get(AGENT_STORAGE_KEY).is_some()
}

/// The same preload shape, declared once per view crate: carried over by its JSON.
fn same_shape<T: Serialize, U: DeserializeOwned + Default>(value: T) -> U {
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .unwrap_or_default()
}

pub(crate) struct SetupBridge {
    storage: Rc<dyn Storage>,
    choice: RefCell<AgentChoiceStore>,
    set_up_before: bool,
    page: Arc<dyn PageBrowser>,
    app: WeakEntity<Browser>,
    profiles: RefCell<Profiles>,
    /// Profiles made to take another browser's cookies: their window opens once the
    /// cookies are in its folder.
    importing: RefCell<HashSet<String>>,
    awaits_cookies: RefCell<Option<BrowserProfile>>,
    /// The proxy of the window the proxy button is about to open.
    next_proxy: RefCell<Option<String>>,
}

impl SetupBridge {
    pub(crate) fn new(
        storage: Rc<dyn Storage>,
        self_check: bool,
        page: Arc<dyn PageBrowser>,
        app: WeakEntity<Browser>,
    ) -> Self {
        Self {
            choice: RefCell::new(AgentChoiceStore::load(storage.as_ref())),
            set_up_before: set_up_before(storage.as_ref(), self_check),
            storage,
            page,
            app,
            profiles: RefCell::new(Profiles::load(&profiles_base())),
            importing: RefCell::default(),
            awaits_cookies: RefCell::default(),
            next_proxy: RefCell::default(),
        }
    }

    /// Open a window of this app on `folder`.
    fn open(&self, spec: WindowSpec) -> Result<(), String> {
        let app = std::env::current_exe().map_err(|error| error.to_string())?;
        open_window(&app, &spec)
    }

    /// The profile's window: on its own folder, or for an incognito one on a folder
    /// that is deleted when the window closes.
    fn profile_window(&self, profile: &BrowserProfile, url: &str) -> WindowSpec {
        let profiles = self.profiles.borrow();
        let folder = profiles.folder(profile);
        WindowSpec {
            ephemeral: folder.is_none(),
            folder: folder.unwrap_or_else(|| ephemeral_folder(&profiles.profiles_dir(), now_ms())),
            url: url.to_owned(),
            proxy: None,
            label: profile.name.clone(),
        }
    }

    /// The import offer waits for the guide to close: it looks again now.
    fn guide_closed(&self, cx: &mut App) {
        let _ = self.app.update(cx, |app, cx| app.setup_changed(cx));
    }
}

impl SetupHost for SetupBridge {
    fn probe_agents(&self, cx: &mut App) -> Task<Vec<AgentHealth>> {
        cx.background_executor()
            .spawn(async { same_shape(crate::chat_agents::probe()) })
    }

    fn set_api_key(
        &self,
        provider: KeyProvider,
        key: String,
        cx: &mut App,
    ) -> Task<Result<(), String>> {
        let provider = match provider {
            KeyProvider::Anthropic => ApiProvider::Anthropic,
            KeyProvider::Openai => ApiProvider::Openai,
            KeyProvider::Grok => ApiProvider::Grok,
            KeyProvider::Glm => ApiProvider::Glm,
        };
        cx.background_executor()
            .spawn(async move { set_api_key(provider, &key) })
    }

    fn browsers(&self, _: &mut App) -> Task<Vec<BrowserInfo>> {
        let chrome = chrome_installed().then(|| BrowserInfo {
            id: BrowserId::Chrome,
            name: "Chrome".to_owned(),
            profiles: vec![BrowserProfileInfo {
                id: "Default".to_owned(),
                name: "Default".to_owned(),
            }],
        });
        Task::ready(chrome.into_iter().collect())
    }

    fn browser_import(
        &self,
        opts: BrowserImportOptions,
        cx: &mut App,
    ) -> Task<Result<BrowserImportResult, String>> {
        if opts.browser != BrowserId::Chrome {
            return Task::ready(Err("only Chrome can be imported yet".to_owned()));
        }
        // Into a new profile: its window has its own jar, so the cookies wait in its
        // folder and the window opens once they are there.
        let waiting = (opts.shared != Some(true))
            .then(|| self.awaits_cookies.borrow_mut().take())
            .flatten();
        if let Some(profile) = waiting {
            let spec = self.profile_window(&profile, IMPORTED_START);
            let app = std::env::current_exe().map_err(|error| error.to_string());
            return cx.background_spawn(async move {
                let hosts = opts.hosts.filter(|hosts| !hosts.is_empty());
                let (cookies, mut result) = read_chrome(opts.profile.as_deref(), hosts.as_deref())?;
                leave_cookies(&spec.folder, &cookies)?;
                open_window(&app?, &spec)?;
                result.imported = cookies.len() as u64;
                Ok(same_shape(result))
            });
        }
        let page = self.page.clone();
        cx.background_spawn(async move {
            import_chrome(page, opts.profile, opts.hosts)
                .await
                .map(same_shape)
        })
    }

    fn list_profiles(&self, _: &mut App) -> Task<Result<Vec<BrowserProfile>, String>> {
        Task::ready(Ok(self.profiles.borrow().list()))
    }

    fn create_profile(
        &self,
        name: String,
        kind: ProfileKind,
        source: Option<String>,
        _: &mut App,
    ) -> Task<Result<BrowserProfile, String>> {
        let imports = source.is_some();
        let created = self
            .profiles
            .borrow_mut()
            .create(&name, kind, source, now_ms());
        if let (Ok(profile), true) = (&created, imports) {
            self.importing.borrow_mut().insert(profile.id.clone());
        }
        Task::ready(created)
    }

    fn delete_profile(&self, id: String, _: &mut App) -> Task<Result<(), String>> {
        Task::ready(self.profiles.borrow_mut().remove(&id))
    }

    // A window is its own process: there is no partition to register for a tab.
    fn register_tab_partition(
        &self,
        _: String,
        _: String,
        _: &mut App,
    ) -> Task<Result<(), String>> {
        Task::ready(Ok(()))
    }

    fn set_proxy(
        &self,
        _: String,
        config: Option<ProxyConfig>,
        partition: Option<String>,
        cx: &mut App,
    ) -> Task<Result<(), String>> {
        let proxy = match config.as_ref().map(proxy_uri).transpose() {
            Ok(proxy) => proxy,
            Err(error) => return Task::ready(Err(error)),
        };
        // With a partition: the proxy of the window `request_navigation` opens next.
        if partition.is_some() {
            *self.next_proxy.borrow_mut() = proxy;
            return Task::ready(Ok(()));
        }
        // Without: this window's own proxy. A process reads it once, so the window
        // starts again; its tabs come back from its storage.
        let asked = restart_with_proxy(&data_dir(), proxy.as_deref());
        if asked.is_ok() {
            cx.defer(|cx| cx.quit());
        }
        Task::ready(asked)
    }

    fn set_active_tab(&self, _: String, _: &mut App) -> Task<Result<(), String>> {
        Task::ready(Ok(()))
    }

    fn onboarded(&self, _: &App) -> bool {
        self.set_up_before || self.choice.borrow().onboarded()
    }

    fn pick_agent(&self, id: AcpAgentId, path: Option<String>, cx: &mut App) {
        let picked = self
            .choice
            .borrow_mut()
            .pick(self.storage.as_ref(), id, path);
        if let Err(error) = picked {
            eprintln!("rever: the agent choice was not saved: {error}");
        }
        let _ = self.app.update(cx, |app, cx| app.choose_agent(id, cx));
        self.guide_closed(cx);
    }

    fn skip_onboarding(&self, cx: &mut App) {
        if let Err(error) = self.choice.borrow_mut().skip(self.storage.as_ref()) {
            eprintln!("rever: skipping the guide was not saved: {error}");
        }
        self.guide_closed(cx);
    }

    fn add_tab(&self, url: &str, opts: AddTabOptions, cx: &mut App) -> String {
        // The proxy button's new window: opened by `request_navigation`, once the
        // proxy is known.
        if opts.proxy.is_some() {
            return PROXY_WINDOW.to_owned();
        }
        let profile = opts
            .profile_id
            .filter(|id| id != DEFAULT_PROFILE_ID)
            .and_then(|id| self.profiles.borrow().find(&id));
        let Some(profile) = profile else {
            let _ = self
                .app
                .update(cx, |app, cx| app.open_tab_from_menu(url, cx));
            return String::new();
        };
        if self.importing.borrow_mut().remove(&profile.id) {
            *self.awaits_cookies.borrow_mut() = Some(profile.clone());
        } else if let Err(error) = self.open(self.profile_window(&profile, url)) {
            eprintln!("rever: {error}");
        }
        profile.id
    }

    fn close_tab(&self, id: &str, _: &mut App) {
        if id == PROXY_WINDOW {
            self.next_proxy.borrow_mut().take();
        }
    }

    // A window's proxy is its process's: nothing is kept per tab.
    fn set_tab_proxy(&self, _: &str, _: Option<ProxyConfig>, _: &mut App) {}

    fn request_navigation(&self, url: &str, cx: &mut App) {
        let Some(proxy) = self.next_proxy.borrow_mut().take() else {
            let _ = self.app.update(cx, |app, cx| app.open_url(url, cx));
            return;
        };
        let opened = self.open(WindowSpec {
            folder: ephemeral_folder(&self.profiles.borrow().profiles_dir(), now_ms()),
            url: url.to_owned(),
            // No profile's: the badge calls it incognito, the chip names the proxy.
            label: String::new(),
            proxy: Some(proxy),
            ephemeral: true,
        });
        if let Err(error) = opened {
            eprintln!("rever: {error}");
        }
    }
}

/// The id `add_tab` answers for the window the proxy button is about to open.
/// What a profile made from another browser's logins opens on.
const IMPORTED_START: &str = "https://www.google.com";

const PROXY_WINDOW: &str = "proxy-window";

/// The folder the profile list and the profiles' folders live in: the first window's
/// data folder, which a profile window's own folder sits two levels under.
fn profiles_base() -> std::path::PathBuf {
    let own = data_dir();
    if this_window().is_none() {
        return own;
    }
    own.parent()
        .and_then(std::path::Path::parent)
        .map_or(own.clone(), std::path::Path::to_owned)
}

#[cfg(test)]
mod tests {
    use rever_ui_chat::model_picker::{AgentHealth as PickerHealth, AgentHealthStatus as Picker};
    use rever_ui_setup::host::AgentHealthStatus;
    use rever_ui_state::storage::MemoryStorage;

    use super::*;

    #[test]
    fn 처음_켠_앱에는_첫_실행_안내를_띄운다() {
        assert!(!set_up_before(&MemoryStorage::new(), false));
    }

    #[test]
    fn 에이전트를_이미_쓰던_사용자에게는_안내를_띄우지_않는다() {
        let storage = MemoryStorage::new();
        storage.set(AGENT_STORAGE_KEY, "codex").unwrap();
        assert!(set_up_before(&storage, false));
    }

    #[test]
    fn 안내를_건너뛴_뒤에는_다시_띄우지_않는다() {
        let storage = MemoryStorage::new();
        AgentChoiceStore::load(&storage).skip(&storage).unwrap();
        assert!(set_up_before(&storage, false));
    }

    #[test]
    fn 자체_점검_실행에서는_안내를_띄우지_않는다() {
        assert!(set_up_before(&MemoryStorage::new(), true));
    }

    #[test]
    fn 고르기_화면의_에이전트_상태를_안내_화면의_모양으로_옮긴다() {
        let picker = PickerHealth {
            id: "claude-code".to_owned(),
            status: Picker::NeedsLogin,
            resolved_path: Some("/usr/local/bin/claude".to_owned()),
            detail: None,
            auto: false,
            plan: Some("max".to_owned()),
        };
        let moved: Vec<AgentHealth> = same_shape(vec![picker]);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].id, "claude-code");
        assert_eq!(moved[0].status, AgentHealthStatus::NeedsLogin);
        assert_eq!(
            moved[0].resolved_path.as_deref(),
            Some("/usr/local/bin/claude")
        );
        assert_eq!(moved[0].plan.as_deref(), Some("max"));
    }
}
