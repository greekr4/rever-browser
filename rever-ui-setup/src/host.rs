//! What the setup views ask of the app around them: the `window.rev.*` calls of the React
//! components (IPC to the main process) and the renderer stores they reached into
//! (tabs, the navigation request, the agent choice). One trait, implemented once by the
//! host; the values are the shapes `src/preload/index.ts` declares, with the same JSON
//! field names.

use gpui::{App, Task};
use rever_ui_state::constants::AcpAgentId;
use rever_ui_state::tabs::{AddTabOptions, ProxyConfig};
use serde::{Deserialize, Serialize};

/// `AgentHealthStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentHealthStatus {
    Ready,
    NotInstalled,
    NeedsKey,
    NeedsLogin,
    AuthFailed,
    Failed,
}

/// One agent's probe result (`AgentHealth`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHealth {
    pub id: String,
    pub status: AgentHealthStatus,
    /// Absolute path of the ACP binary, or null for API providers.
    pub resolved_path: Option<String>,
    /// Short renderable reason. Never contains a token or key.
    pub detail: Option<String>,
    /// True when an existing CLI login covers this agent — no setup needed.
    pub auto: bool,
    /// Plan the existing login advertises ('max', 'chatgpt'), or null.
    pub plan: Option<String>,
}

/// The providers `window.rev.settings.setApiKey` takes a key for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyProvider {
    Anthropic,
    Openai,
    Grok,
    Glm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    Persistent,
    Incognito,
}

/// A named browsing profile. Mirrors main/tab-partition.ts BrowserProfile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserProfile {
    pub id: String,
    pub name: String,
    pub kind: ProfileKind,
    pub partition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Mirrors main/browser-cookie-import.ts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserId {
    Chrome,
    Edge,
    Brave,
    Arc,
    Chromium,
    Vivaldi,
    Firefox,
    Safari,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserProfileInfo {
    pub id: String,
    pub name: String,
}

/// A detected browser (installed, with importable profiles).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserInfo {
    pub id: BrowserId,
    pub name: String,
    pub profiles: Vec<BrowserProfileInfo>,
}

/// The argument of `window.rev.storage.browserImport`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserImportOptions {
    pub browser: BrowserId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<bool>,
}

/// What `window.rev.storage.browserImport` resolves to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserImportResult {
    pub ok: bool,
    pub imported: u64,
    pub skipped: u64,
    pub undecryptable: u64,
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where the views get what the React components took from `window.rev` and the stores.
/// An `Err` is a rejected promise, with the error's message.
pub trait SetupHost {
    // ── window.rev.acp / window.rev.settings ────────────────────────────────

    /// Probe every agent for real (ACP handshake / authenticated API call). A failure is
    /// reported per agent, as a status; the call itself does not fail.
    fn probe_agents(&self, cx: &mut App) -> Task<Vec<AgentHealth>>;
    fn set_api_key(
        &self,
        provider: KeyProvider,
        key: String,
        cx: &mut App,
    ) -> Task<Result<(), String>>;

    // ── window.rev.storage ──────────────────────────────────────────────────

    /// Detected browsers; empty when none is found.
    fn browsers(&self, cx: &mut App) -> Task<Vec<BrowserInfo>>;
    /// Copy a browser profile's cookies into the active session (or the shared one).
    fn browser_import(
        &self,
        opts: BrowserImportOptions,
        cx: &mut App,
    ) -> Task<Result<BrowserImportResult, String>>;

    // ── window.rev.profiles ─────────────────────────────────────────────────

    fn list_profiles(&self, cx: &mut App) -> Task<Result<Vec<BrowserProfile>, String>>;
    fn create_profile(
        &self,
        name: String,
        kind: ProfileKind,
        source: Option<String>,
        cx: &mut App,
    ) -> Task<Result<BrowserProfile, String>>;
    fn delete_profile(&self, id: String, cx: &mut App) -> Task<Result<(), String>>;
    /// Register a profile tab's partition (no proxy) so cookie import / current-ip
    /// target that profile's session.
    fn register_tab_partition(
        &self,
        tab_id: String,
        partition: String,
        cx: &mut App,
    ) -> Task<Result<(), String>>;

    // ── window.rev.proxy ────────────────────────────────────────────────────

    /// Apply (or clear, with `None`) the tab's upstream proxy. `partition` is passed once,
    /// when creating an isolated proxy tab, so the tab→partition mapping is registered
    /// before the proxy is applied.
    fn set_proxy(
        &self,
        tab_id: String,
        config: Option<ProxyConfig>,
        partition: Option<String>,
        cx: &mut App,
    ) -> Task<Result<(), String>>;
    /// Tell the main side which tab is active so cookie import targets its partition.
    fn set_active_tab(&self, tab_id: String, cx: &mut App) -> Task<Result<(), String>>;

    // ── renderer stores ─────────────────────────────────────────────────────

    /// `useAgentChoice`'s `onboarded`: true once an agent was picked or onboarding skipped.
    fn onboarded(&self, cx: &App) -> bool;
    /// `useAgentChoice().pick(id, path)`.
    fn pick_agent(&self, id: AcpAgentId, path: Option<String>, cx: &mut App);
    /// `useAgentChoice().skip()`.
    fn skip_onboarding(&self, cx: &mut App);
    /// `useTabsStore.getState().addTab(url, opts)`; returns the new tab's id.
    fn add_tab(&self, url: &str, opts: AddTabOptions, cx: &mut App) -> String;
    fn close_tab(&self, id: &str, cx: &mut App);
    fn set_tab_proxy(&self, id: &str, proxy: Option<ProxyConfig>, cx: &mut App);
    /// `useNavigationRequestStore().request(url)`: navigate the active tab.
    fn request_navigation(&self, url: &str, cx: &mut App);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn 에이전트_상태는_프리로드와_같은_필드_이름으로_읽고_쓴다() {
        let health: AgentHealth = serde_json::from_value(json!({
            "id": "claude-code",
            "status": "needs-login",
            "resolvedPath": "/usr/local/bin/claude",
            "detail": null,
            "auto": false,
            "plan": "max"
        }))
        .unwrap();
        assert_eq!(health.status, AgentHealthStatus::NeedsLogin);
        assert_eq!(
            health.resolved_path.as_deref(),
            Some("/usr/local/bin/claude")
        );
        assert_eq!(
            serde_json::to_value(&health).unwrap()["resolvedPath"],
            "/usr/local/bin/claude"
        );
        for (status, name) in [
            (AgentHealthStatus::Ready, "ready"),
            (AgentHealthStatus::NotInstalled, "not-installed"),
            (AgentHealthStatus::NeedsKey, "needs-key"),
            (AgentHealthStatus::AuthFailed, "auth-failed"),
            (AgentHealthStatus::Failed, "failed"),
        ] {
            assert_eq!(serde_json::to_value(status).unwrap(), name);
        }
    }

    #[test]
    fn 가져오기_옵션은_없는_필드를_빼고_보낸다() {
        let opts = BrowserImportOptions {
            browser: BrowserId::Chrome,
            profile: Some("Default".to_owned()),
            hosts: Some(vec![]),
            shared: None,
        };
        assert_eq!(
            serde_json::to_value(&opts).unwrap(),
            json!({ "browser": "chrome", "profile": "Default", "hosts": [] })
        );
    }

    #[test]
    fn 프로필은_source가_없어도_읽힌다() {
        let profile: BrowserProfile = serde_json::from_value(json!({
            "id": "default", "name": "Default", "kind": "persistent",
            "partition": "persist:rever-shared"
        }))
        .unwrap();
        assert_eq!(profile.source, None);
        assert_eq!(profile.kind, ProfileKind::Persistent);
        assert_eq!(
            serde_json::to_value(KeyProvider::Anthropic).unwrap(),
            "anthropic"
        );
    }

    #[test]
    fn 가져오기_결과는_error가_없어도_읽힌다() {
        let result: BrowserImportResult = serde_json::from_value(json!({
            "ok": true, "imported": 12, "skipped": 1, "undecryptable": 0, "total": 13
        }))
        .unwrap();
        assert!(result.ok);
        assert_eq!((result.imported, result.total), (12, 13));
        assert_eq!(result.error, None);
    }
}
