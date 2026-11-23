//! The agent behind a workflow's AI step (`__ai_prompt`): one of the chat's agents,
//! started for the run and driven to the end of each prompt.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rever_chat_core::acp_permission::{RequestPermissionOutcome, RequestPermissionResponse};
use rever_mcp::ToolProfileState;
use rever_tools_api::tool::BoxFuture;
use rever_tools_automation::workflow_executor::{AgentDef, AiAgents};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::agent::{AgentEvent, ChatAgent};
use crate::api_keys::{ApiProvider, api_key};
use crate::chat_agents::{agent_by_id, spawn_agent};

struct Session {
    agent: Box<dyn ChatAgent>,
    /// Set by the agent's threads whenever it has something new.
    woken: Arc<Notify>,
}

/// The sessions of the workflows that are running.
pub struct WorkflowAgents {
    /// The app's MCP server as (url, Authorization header): the step's agent gets the
    /// app's tools, as the chat's agent does.
    mcp: Option<(String, String)>,
    profile: Option<Arc<ToolProfileState>>,
    sessions: Mutex<HashMap<String, Session>>,
    next: AtomicU64,
}

impl WorkflowAgents {
    pub fn new(mcp: Option<(String, String)>, profile: Option<Arc<ToolProfileState>>) -> Self {
        Self {
            mcp,
            profile,
            sessions: Mutex::default(),
            next: AtomicU64::new(1),
        }
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<String, Session>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A piece of the agent's answer as the session notification the executor reads.
fn answer_chunk(text: &str) -> Value {
    json!({ "update": {
        "sessionUpdate": "agent_message_chunk",
        "content": { "type": "text", "text": text },
    } })
}

fn provider(name: &str) -> Option<ApiProvider> {
    Some(match name {
        "anthropic" => ApiProvider::Anthropic,
        "openai" => ApiProvider::Openai,
        "grok" => ApiProvider::Grok,
        "glm" => ApiProvider::Glm,
        _ => return None,
    })
}

/// Send `prompt` and pass the answer on piece by piece until the turn ends.
async fn run_prompt(
    session: &mut Session,
    prompt: &str,
    on_update: &mut (dyn FnMut(&Value) + Send),
) -> Result<(), String> {
    session.agent.send(prompt)?;
    loop {
        for event in session.agent.drain() {
            match event {
                AgentEvent::Text(text) => on_update(&answer_chunk(&text)),
                AgentEvent::ToolCall(..) | AgentEvent::ToolResult(..) => {}
                AgentEvent::Done => return Ok(()),
                AgentEvent::Failed(error) => return Err(error),
                // A workflow runs unattended: there is nobody to ask.
                AgentEvent::Permission(ask) => (ask.answer)(RequestPermissionResponse {
                    outcome: RequestPermissionOutcome::Cancelled,
                }),
            }
        }
        session.woken.notified().await;
    }
}

impl AiAgents for WorkflowAgents {
    fn has_api_key(&self, name: &str) -> bool {
        provider(name).is_some_and(|provider| api_key(provider).is_some())
    }

    fn spawn_session<'a>(
        &'a self,
        agent: &'a AgentDef,
        cwd: &'a str,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let id =
                agent_by_id(&agent.id).ok_or_else(|| format!("unknown agent: {}", agent.id))?;
            let woken = Arc::new(Notify::new());
            let wake = {
                let woken = woken.clone();
                move || woken.notify_one()
            };
            let mcp = self
                .mcp
                .as_ref()
                .map(|(url, auth)| (url.as_str(), auth.as_str()));
            let agent = spawn_agent(id, None, Path::new(cwd), mcp, self.profile.clone(), wake)?;
            let key = format!("workflow-{}", self.next.fetch_add(1, Ordering::Relaxed));
            self.sessions()
                .insert(key.clone(), Session { agent, woken });
            Ok(key)
        })
    }

    fn prompt_session<'a>(
        &'a self,
        session_id: &'a str,
        prompt: &'a str,
        on_update: &'a mut (dyn FnMut(&Value) + Send),
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            // Taken out for the prompt, so the lock is not held while it waits.
            let Some(mut session) = self.sessions().remove(session_id) else {
                return Err("no agent session".to_owned());
            };
            let outcome = run_prompt(&mut session, prompt, on_update).await;
            self.sessions().insert(session_id.to_owned(), session);
            outcome
        })
    }

    fn kill_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            // Dropping the session ends its process.
            self.sessions().remove(session_id);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::PermissionAsk;

    /// An agent that says what it was scripted to say, all at once.
    struct Scripted {
        events: Vec<AgentEvent>,
        sent: Arc<Mutex<Vec<String>>>,
    }

    impl ChatAgent for Scripted {
        fn send(&mut self, text: &str) -> Result<(), String> {
            self.sent.lock().unwrap().push(text.to_owned());
            Ok(())
        }
        fn drain(&mut self) -> Vec<AgentEvent> {
            std::mem::take(&mut self.events)
        }
        fn set_model(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn session(events: Vec<AgentEvent>) -> (Session, Arc<Mutex<Vec<String>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let agent = Box::new(Scripted {
            events,
            sent: sent.clone(),
        });
        let woken = Arc::new(Notify::new());
        (Session { agent, woken }, sent)
    }

    /// Runs a prompt and returns how it ended and the text pieces passed on.
    async fn prompt(events: Vec<AgentEvent>) -> (Result<(), String>, Vec<String>) {
        let (mut session, _) = session(events);
        let mut pieces = Vec::new();
        let mut on_update = |update: &Value| {
            let text = update["update"]["content"]["text"]
                .as_str()
                .unwrap_or_default();
            pieces.push(text.to_owned());
        };
        let outcome = run_prompt(&mut session, "질문", &mut on_update).await;
        (outcome, pieces)
    }

    #[tokio::test]
    async fn 답의_조각을_차례로_넘기고_턴이_끝나면_성공한다() {
        let events = vec![
            AgentEvent::Text("안".into()),
            AgentEvent::Text("녕".into()),
            AgentEvent::Done,
        ];
        assert_eq!(
            prompt(events).await,
            (Ok(()), vec!["안".to_owned(), "녕".to_owned()])
        );
    }

    #[tokio::test]
    async fn 에이전트가_실패하면_그_이유로_끝난다() {
        let events = vec![AgentEvent::Failed("키가 없습니다".into())];
        assert_eq!(prompt(events).await.0, Err("키가 없습니다".to_owned()));
    }

    #[tokio::test]
    async fn 프롬프트는_에이전트에게_그대로_보낸다() {
        let (mut session, sent) = session(vec![AgentEvent::Done]);
        run_prompt(&mut session, "요약해 줘", &mut |_| {})
            .await
            .unwrap();
        assert_eq!(*sent.lock().unwrap(), ["요약해 줘"]);
    }

    #[tokio::test]
    async fn 권한을_묻는_요청은_물어볼_사람이_없어_취소로_답한다() {
        let answered = Arc::new(Mutex::new(None));
        let ask = PermissionAsk {
            request: json!({}),
            answer: Box::new({
                let answered = answered.clone();
                move |response| *answered.lock().unwrap() = Some(response.outcome)
            }),
        };
        let events = vec![AgentEvent::Permission(ask), AgentEvent::Done];
        prompt(events).await.0.unwrap();
        assert_eq!(
            *answered.lock().unwrap(),
            Some(RequestPermissionOutcome::Cancelled)
        );
    }

    #[test]
    fn 답_조각은_실행기가_읽는_세션_알림_모양이다() {
        let update = &answer_chunk("x")["update"];
        assert_eq!(
            (
                update["sessionUpdate"].as_str(),
                update["content"]["type"].as_str()
            ),
            (Some("agent_message_chunk"), Some("text"))
        );
    }

    #[test]
    fn 키를_쓰는_제공자가_아니면_키가_없는_것으로_본다() {
        let agents = WorkflowAgents::new(None, None);
        assert!(!agents.has_api_key("claude-code"));
    }
}
