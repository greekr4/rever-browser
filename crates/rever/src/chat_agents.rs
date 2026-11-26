//! Port of `src/main/agent-router.ts` and the part of `agent-probe.ts` the model picker
//! shows: which kind of session runs which agent, which agents can run on this
//! machine, and the picker's host.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Task};
use rever_agent::acp_detect::{AgentProbe, detect_agents};
use rever_agent::claude_cli::ClaudeCliFinder;
use rever_agent::grok_login::{TokenResponse, read_grok_login_file, resolve_grok_credential};
use rever_control_server::ToolProfileState;
use rever_ui_chat::model_catalog::{LiveModel, ModelCatalogStore};
use rever_ui_chat::model_picker::{AgentHealth, AgentHealthStatus, KeyProvider, ModelPickerHost};
use rever_ui_state::constants::{ACP_AGENTS, AcpAgentId};

use crate::agent::{AgentSession, ChatAgent};
use crate::agent_acp::AcpSession;
use crate::agent_api::{ApiConfig, ApiSession};
use crate::api_keys::{ApiProvider, api_key, set_api_key};

/// The binary that speaks ACP for Codex.
const CODEX_ACP: &str = "codex-acp";

/// The agent with this id, as the picker and the storage name it.
pub fn agent_by_id(id: &str) -> Option<AcpAgentId> {
    ACP_AGENTS
        .iter()
        .map(|agent| agent.id)
        .find(|agent| agent.as_str() == id)
}

fn codex_acp_path() -> Option<String> {
    let probe = AgentProbe {
        command: CODEX_ACP.to_owned(),
        fallback_bins: None,
    };
    detect_agents(&[probe]).into_iter().next()?.resolved_path
}

/// The xAI credential: a `grok login` subscription first, else the saved API key.
fn grok_key() -> Option<String> {
    let saved = api_key(ApiProvider::Grok);
    let refresh = |request: &rever_agent::grok_login::GrokRefreshRequest| {
        let response = reqwest::blocking::Client::new()
            .post(&request.url)
            .header(
                "Content-Type",
                rever_agent::grok_login::GrokRefreshRequest::CONTENT_TYPE,
            )
            .body(request.body.clone())
            .send()
            .ok()?;
        let body = serde_json::from_str::<serde_json::Value>(&response.text().ok()?).ok()?;
        Some(TokenResponse::from_value(&body))
    };
    Some(resolve_grok_credential(saved.as_deref(), refresh)?.token)
}

/// Start a session with `agent`. `model` is the picker's choice, `None` for the
/// agent's own default; `mcp` is the app's MCP server as (url, Authorization header).
pub fn spawn_agent(
    agent: AcpAgentId,
    model: Option<&str>,
    cwd: &Path,
    mcp: Option<(&str, &str)>,
    profile: Option<Arc<ToolProfileState>>,
    wake: impl Fn() + Send + Sync + 'static,
) -> Result<Box<dyn ChatAgent>, String> {
    let (config, key): (ApiConfig, fn() -> Option<String>) = match agent {
        AcpAgentId::ClaudeCode => {
            // `default` is the picker's name for "whatever the CLI is set to".
            let model = model.filter(|model| *model != "default");
            return Ok(Box::new(AgentSession::spawn(cwd, mcp, model, wake)?));
        }
        AcpAgentId::Codex => {
            let command = codex_acp_path().unwrap_or_else(|| CODEX_ACP.to_owned());
            return Ok(Box::new(AcpSession::spawn(
                &command, cwd, mcp, model, wake,
            )?));
        }
        AcpAgentId::Anthropic => (ApiConfig::ANTHROPIC, || api_key(ApiProvider::Anthropic)),
        AcpAgentId::Openai => (ApiConfig::OPENAI, || api_key(ApiProvider::Openai)),
        AcpAgentId::Glm => (ApiConfig::GLM, || api_key(ApiProvider::Glm)),
        AcpAgentId::Grok => (ApiConfig::GROK, grok_key),
    };
    Ok(Box::new(ApiSession::new(
        config, model, key, mcp, profile, wake,
    )))
}

/// What stands between this machine and an agent. A missing piece is found by looking
/// for it (a binary, a login file, a key), not by starting the agent.
// shortcut: presence only, so a broken install or a mistyped key reads "ready" until
// the first turn fails; upgrade to the Electron probe's handshake and authenticated GET
// when that shows up as a problem.
fn status(agent: AcpAgentId) -> (AgentHealthStatus, Option<String>) {
    let keyed = |provider| match api_key(provider) {
        Some(_) => (AgentHealthStatus::Ready, None),
        None => (AgentHealthStatus::NeedsKey, None),
    };
    match agent {
        AcpAgentId::Anthropic => keyed(ApiProvider::Anthropic),
        AcpAgentId::Openai => keyed(ApiProvider::Openai),
        AcpAgentId::Glm => keyed(ApiProvider::Glm),
        AcpAgentId::Grok if read_grok_login_file().logged_in => (AgentHealthStatus::Ready, None),
        AcpAgentId::Grok => keyed(ApiProvider::Grok),
        AcpAgentId::ClaudeCode => match ClaudeCliFinder::new().find() {
            Some(path) => (AgentHealthStatus::Ready, Some(path.to_owned())),
            None => (AgentHealthStatus::NotInstalled, None),
        },
        AcpAgentId::Codex => match codex_acp_path() {
            None => (AgentHealthStatus::NotInstalled, None),
            Some(path) => {
                let signed_in = std::env::home_dir()
                    .is_some_and(|home| home.join(".codex").join("auth.json").is_file());
                let status = if signed_in {
                    AgentHealthStatus::Ready
                } else {
                    AgentHealthStatus::NeedsLogin
                };
                (status, Some(path))
            }
        },
    }
}

/// Every agent's health, for the picker. Reads the keychain and the disk: run it off
/// the main thread.
pub fn probe() -> Vec<AgentHealth> {
    ACP_AGENTS
        .iter()
        .map(|agent| {
            let (status, resolved_path) = status(agent.id);
            AgentHealth {
                id: agent.id.as_str().to_owned(),
                status,
                // A CLI's own login covers it: nothing to set up.
                auto: resolved_path.is_some() && status == AgentHealthStatus::Ready,
                resolved_path,
                detail: None,
                plan: None,
            }
        })
        .collect()
}

/// What the model picker asks of the app.
pub struct PickerHost {
    /// The model lists; the app fills in what the CLIs report.
    pub models: Rc<RefCell<ModelCatalogStore>>,
}

impl ModelPickerHost for PickerHost {
    fn probe(&self, cx: &mut App) -> Task<Vec<AgentHealth>> {
        cx.background_executor().spawn(async { probe() })
    }

    fn set_api_key(
        &self,
        provider: KeyProvider,
        key: String,
        cx: &mut App,
    ) -> Task<Result<bool, String>> {
        let provider = match provider {
            KeyProvider::Anthropic => ApiProvider::Anthropic,
            KeyProvider::Openai => ApiProvider::Openai,
            KeyProvider::Grok => ApiProvider::Grok,
            KeyProvider::Glm => ApiProvider::Glm,
        };
        cx.background_executor()
            .spawn(async move { set_api_key(provider, &key).map(|()| true) })
    }

    fn models(&self, _: &App) -> Vec<LiveModel> {
        self.models.borrow().all_agent_models()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 저장된_아이디로_에이전트를_찾는다() {
        assert_eq!(agent_by_id("codex"), Some(AcpAgentId::Codex));
        assert_eq!(agent_by_id("claude-code"), Some(AcpAgentId::ClaudeCode));
        assert_eq!(agent_by_id("gemini"), None);
        assert_eq!(agent_by_id(""), None);
    }

    #[test]
    fn 점검_결과는_모든_에이전트를_한_번씩_담는다() {
        let health = probe();
        let ids: Vec<&str> = health.iter().map(|agent| agent.id.as_str()).collect();
        let expected: Vec<&str> = ACP_AGENTS.iter().map(|agent| agent.id.as_str()).collect();
        assert_eq!(ids, expected);
    }
}
