//! What the first-run views ask of the app: which agents can run here, where an API
//! key goes, Chrome's cookies, and the agent the user picks.
//!
//! Profiles and per-tab proxies have no engine under them (one cookie jar, one proxy
//! read at startup), so their calls answer with an error and their views stay unmounted.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Task, WeakEntity, prelude::*};
use rever_tools_api::tool::Browser as PageBrowser;
use rever_ui_setup::host::{
    AgentHealth, BrowserId, BrowserImportOptions, BrowserImportResult, BrowserInfo, BrowserProfile,
    BrowserProfileInfo, KeyProvider, ProfileKind, SetupHost,
};
use rever_ui_state::Storage;
use rever_ui_state::agent_choice::{AgentChoiceStore, has_onboarded};
use rever_ui_state::constants::AcpAgentId;
use rever_ui_state::tabs::{AddTabOptions, ProxyConfig};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::api_keys::{ApiProvider, set_api_key};
use crate::app::{AGENT_STORAGE_KEY, Browser};
use crate::storage_host::{chrome_installed, import_chrome};

const NO_ENGINE: &str = "profiles and per-tab proxies are not available in this engine";

/// Whether this user is past the first run: an agent was picked or the guide skipped,
/// here or before the guide existed. A self-check run never shows it.
fn set_up_before(storage: &dyn Storage, self_check: bool) -> bool {
    self_check || has_onboarded(storage) || storage.get(AGENT_STORAGE_KEY).is_some()
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
        let page = self.page.clone();
        cx.background_spawn(async move {
            import_chrome(page, opts.profile, opts.hosts)
                .await
                .map(same_shape)
        })
    }

    fn list_profiles(&self, _: &mut App) -> Task<Result<Vec<BrowserProfile>, String>> {
        Task::ready(Err(NO_ENGINE.to_owned()))
    }

    fn create_profile(
        &self,
        _: String,
        _: ProfileKind,
        _: Option<String>,
        _: &mut App,
    ) -> Task<Result<BrowserProfile, String>> {
        Task::ready(Err(NO_ENGINE.to_owned()))
    }

    fn delete_profile(&self, _: String, _: &mut App) -> Task<Result<(), String>> {
        Task::ready(Err(NO_ENGINE.to_owned()))
    }

    fn register_tab_partition(
        &self,
        _: String,
        _: String,
        _: &mut App,
    ) -> Task<Result<(), String>> {
        Task::ready(Err(NO_ENGINE.to_owned()))
    }

    fn set_proxy(
        &self,
        _: String,
        _: Option<ProxyConfig>,
        _: Option<String>,
        _: &mut App,
    ) -> Task<Result<(), String>> {
        Task::ready(Err(NO_ENGINE.to_owned()))
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

    // The two views that are mounted open no tabs and set no proxy.
    fn add_tab(&self, _: &str, _: AddTabOptions, _: &mut App) -> String {
        String::new()
    }

    fn close_tab(&self, _: &str, _: &mut App) {}

    fn set_tab_proxy(&self, _: &str, _: Option<ProxyConfig>, _: &mut App) {}

    fn request_navigation(&self, _: &str, _: &mut App) {}
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
