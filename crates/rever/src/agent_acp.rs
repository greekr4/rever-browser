//! Port of `src/main/acp-session.ts`: a chat session with an agent that speaks the
//! Agent Client Protocol over stdio (Codex, through `codex-acp`).
//!
//! JSON-RPC 2.0, one message per line. Nothing here blocks: the handshake
//! (`initialize`, then `session/new`) advances as the answers arrive in
//! [`AcpSession::drain`], and a prompt sent before the session exists waits for it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use rever_agent::acp_models::model_state_from_session;
use rever_agent::agent_env::agent_env;
use rever_agent::claude_cli::ClaudeCliFinder;
use rever_tool_profile::risky_tools::fallback_option;
use serde_json::{Value, json};

use crate::agent::{AgentEvent, ChatAgent};

/// The version of the protocol this client speaks.
const PROTOCOL_VERSION: u64 = 1;
/// Name the agent sees the app's MCP server under, as in the Electron app.
const MCP_SERVER_NAME: &str = "rever-traffic";

/// Ids of the requests whose answers move the session forward.
const INITIALIZE: u64 = 1;
const NEW_SESSION: u64 = 2;

pub struct AcpSession {
    child: Child,
    stdin: ChildStdin,
    inbox: Arc<Mutex<Vec<Value>>>,
    state: AcpState,
}

/// Everything but the process: what [`AcpState::handle`] turns a message into.
struct AcpState {
    cwd: String,
    mcp: Option<(String, String)>,
    session_id: Option<String>,
    /// A prompt sent before `session/new` was answered.
    waiting_prompt: Option<String>,
    /// Id of the `session/prompt` in flight.
    prompt_id: Option<u64>,
    next_id: u64,
    /// The model to use once the session exists, and how the agent takes it.
    model: Option<String>,
    model_config_id: Option<String>,
    tool_names: HashMap<String, String>,
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

impl AcpState {
    fn new(cwd: &Path, mcp: Option<(&str, &str)>, model: Option<&str>) -> Self {
        Self {
            cwd: cwd.to_string_lossy().into_owned(),
            mcp: mcp.map(|(url, auth)| (url.to_owned(), auth.to_owned())),
            session_id: None,
            waiting_prompt: None,
            prompt_id: None,
            next_id: NEW_SESSION + 1,
            model: model.map(str::to_owned),
            model_config_id: None,
            tool_names: HashMap::new(),
        }
    }

    fn initialize(&self) -> Value {
        let params = json!({ "protocolVersion": PROTOCOL_VERSION, "clientCapabilities": {} });
        request(INITIALIZE, "initialize", params)
    }

    fn new_session(&self) -> Value {
        let servers: Vec<Value> = self
            .mcp
            .iter()
            .map(|(url, auth)| {
                json!({
                    "type": "http",
                    "name": MCP_SERVER_NAME,
                    "url": url,
                    "headers": [
                        { "name": "Authorization", "value": auth },
                        { "name": "X-Rever-Catalog", "value": "follow" },
                    ],
                })
            })
            .collect();
        request(NEW_SESSION, "session/new", json!({ "cwd": self.cwd, "mcpServers": servers }))
    }

    fn prompt(&mut self, session_id: &str, text: &str) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.prompt_id = Some(id);
        let params = json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": text }] });
        request(id, "session/prompt", params)
    }

    /// The request that makes the session use `model`: through the model config option
    /// when the agent has one, else the older `session/set_model`.
    fn set_model(&mut self, session_id: &str, model: &str) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        match &self.model_config_id {
            Some(config) => request(
                id,
                "session/set_config_option",
                json!({ "sessionId": session_id, "configId": config, "value": model }),
            ),
            None => request(
                id,
                "session/set_model",
                json!({ "sessionId": session_id, "modelId": model }),
            ),
        }
    }

    /// One message from the agent: what to write back, and what the chat should see.
    fn handle(&mut self, message: &Value) -> (Vec<Value>, Vec<AgentEvent>) {
        let (mut out, mut events) = (Vec::new(), Vec::new());
        let id = message.get("id").filter(|id| !id.is_null());
        match (message["method"].as_str(), id) {
            // The agent asks before a tool runs. Risky tools are refused: nothing here
            // shows the user what would run.
            (Some("session/request_permission"), Some(id)) => {
                let option = fallback_option(&message["params"]);
                let outcome = json!({ "outcome": "selected", "optionId": option });
                out.push(json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": outcome } }));
            }
            // Anything else it asks of the client (files, terminals) is not offered.
            (Some(method), Some(id)) => {
                let error = json!({ "code": -32601, "message": format!("{method} is not supported") });
                out.push(json!({ "jsonrpc": "2.0", "id": id, "error": error }));
            }
            (Some("session/update"), None) => {
                let update = &message["params"]["update"];
                let call_id = update["toolCallId"].as_str().unwrap_or_default();
                match update["sessionUpdate"].as_str() {
                    Some("agent_message_chunk") => {
                        if let Some(text) = update["content"]["text"].as_str() {
                            events.push(AgentEvent::Text(text.to_owned()));
                        }
                    }
                    // The call is announced once, with its name; its end is the update
                    // that carries a final status.
                    Some("tool_call") => {
                        let name = update["title"].as_str().unwrap_or("tool").to_owned();
                        self.tool_names.insert(call_id.to_owned(), name.clone());
                        events.push(AgentEvent::ToolCall(name, update["rawInput"].clone()));
                    }
                    Some("tool_call_update") => {
                        let finished = matches!(update["status"].as_str(), Some("completed" | "failed"));
                        if let Some(name) = self.tool_names.get(call_id).filter(|_| finished) {
                            let output = match &update["rawOutput"] {
                                Value::String(text) => text.clone(),
                                Value::Null => String::new(),
                                other => other.to_string(),
                            };
                            events.push(AgentEvent::ToolResult(name.clone(), output));
                        }
                    }
                    _ => {}
                }
            }
            (Some(_), None) => {}
            (None, _) => self.answer(message, &mut out, &mut events),
        }
        (out, events)
    }

    /// The agent answered one of this client's requests.
    fn answer(&mut self, message: &Value, out: &mut Vec<Value>, events: &mut Vec<AgentEvent>) {
        let id = message["id"].as_u64();
        let failure = message.get("error").map(|error| {
            let text = error["message"].as_str().unwrap_or("the agent reported an error");
            // codex-acp puts the cause (not signed in, no access to the model) in `data`.
            match error["data"].as_str().or_else(|| error["data"]["message"].as_str()) {
                // codex-acp often repeats the cause in both fields.
                Some(detail) if !text.contains(detail) => format!("{text}: {detail}"),
                _ => text.to_owned(),
            }
        });
        if id == Some(INITIALIZE) {
            match failure {
                Some(error) => events.push(AgentEvent::Failed(error)),
                None => out.push(self.new_session()),
            }
        } else if id == Some(NEW_SESSION) {
            if let Some(error) = failure {
                events.push(AgentEvent::Failed(error));
                return;
            }
            let result = &message["result"];
            let Some(session_id) = result["sessionId"].as_str().map(str::to_owned) else {
                events.push(AgentEvent::Failed("the agent opened no session".to_owned()));
                return;
            };
            // Any other mode can refuse the app's tools without asking.
            if result["modes"]["currentModeId"].as_str().is_some_and(|mode| mode != "default") {
                let id = self.next_id;
                self.next_id += 1;
                let params = json!({ "sessionId": session_id, "modeId": "default" });
                out.push(request(id, "session/set_mode", params));
            }
            let models = model_state_from_session(result);
            self.model_config_id = models.model_config_id;
            if let Some(model) = self.model.clone().filter(|model| models.current_model_id.as_ref() != Some(model)) {
                out.push(self.set_model(&session_id, &model));
            }
            if let Some(text) = self.waiting_prompt.take() {
                out.push(self.prompt(&session_id, &text));
            }
            self.session_id = Some(session_id);
        } else if id.is_some() && id == self.prompt_id {
            self.prompt_id = None;
            events.push(match failure {
                Some(error) => AgentEvent::Failed(error),
                None => AgentEvent::Done,
            });
        }
        // Answers to set_mode and set_model need nothing done.
    }
}

impl AcpSession {
    /// Start `command` (the agent's ACP binary) and begin the handshake. `wake` is
    /// called from a reader thread whenever [`AcpSession::drain`] has something new.
    pub fn spawn(
        command: &str,
        cwd: &Path,
        mcp: Option<(&str, &str)>,
        model: Option<&str>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(cwd).map_err(|error| format!("{}: {error}", cwd.display()))?;
        let mut child = Command::new(command)
            .current_dir(cwd)
            .env_clear()
            .envs(agent_env(command, &ClaudeCliFinder::new()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("{command} could not be started ({error})"))?;
        let (Some(stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(format!("{command} started without its pipes"));
        };
        let inbox = Arc::new(Mutex::new(Vec::new()));
        let stderr_text = Arc::new(Mutex::new(String::new()));
        {
            // Drained so a chatty agent cannot block on a full pipe; kept for the exit message.
            let stderr_text = stderr_text.clone();
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = stderr.read_to_string(&mut text);
                *stderr_text.lock().unwrap() = text;
            });
        }
        {
            let inbox = inbox.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Ok(message) = serde_json::from_str::<Value>(&line) {
                        inbox.lock().unwrap().push(message);
                        wake();
                    }
                }
                // stdout closed: the agent is gone. Give stderr a moment to be collected.
                std::thread::sleep(std::time::Duration::from_millis(100));
                let detail = stderr_text.lock().unwrap().trim().to_owned();
                inbox.lock().unwrap().push(json!({ "rever_exit": detail }));
                wake();
            });
        }
        let state = AcpState::new(cwd, mcp, model);
        let mut session = Self { child, stdin, inbox, state };
        let hello = session.state.initialize();
        session.write(&[hello]).map_err(|error| format!("{command} is not accepting input: {error}"))?;
        Ok(session)
    }

    fn write(&mut self, messages: &[Value]) -> std::io::Result<()> {
        for message in messages {
            self.stdin.write_all(format!("{message}\n").as_bytes())?;
        }
        self.stdin.flush()
    }
}

impl ChatAgent for AcpSession {
    fn send(&mut self, text: &str) -> Result<(), String> {
        let Some(session_id) = self.state.session_id.clone() else {
            self.state.waiting_prompt = Some(text.to_owned());
            return Ok(());
        };
        let prompt = self.state.prompt(&session_id, text);
        self.write(&[prompt]).map_err(|error| error.to_string())
    }

    fn drain(&mut self) -> Vec<AgentEvent> {
        let messages = std::mem::take(&mut *self.inbox.lock().unwrap());
        let mut events = Vec::new();
        for message in messages {
            if let Some(detail) = message["rever_exit"].as_str() {
                let last = detail.lines().last().unwrap_or("the agent exited");
                events.push(AgentEvent::Failed(last.to_owned()));
                continue;
            }
            let (out, mut new_events) = self.state.handle(&message);
            if self.write(&out).is_err() {
                new_events.push(AgentEvent::Failed("the agent is not accepting input".to_owned()));
            }
            events.append(&mut new_events);
        }
        events
    }

    fn set_model(&mut self, model: &str) -> Result<(), String> {
        self.state.model = Some(model.to_owned());
        let Some(session_id) = self.state.session_id.clone() else {
            return Ok(());
        };
        let request = self.state.set_model(&session_id, model);
        self.write(&[request]).map_err(|error| error.to_string())
    }
}

impl Drop for AcpSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AcpState {
        AcpState::new(Path::new("/tmp/w"), Some(("http://127.0.0.1:9/mcp", "Bearer t")), None)
    }

    fn methods(messages: &[Value]) -> Vec<&str> {
        messages.iter().map(|m| m["method"].as_str().unwrap_or("")).collect()
    }

    #[test]
    fn 초기화_응답_뒤에_mcp_서버를_붙인_세션을_연다() {
        let mut state = state();
        let (out, events) = state.handle(&json!({ "jsonrpc": "2.0", "id": 1, "result": {} }));
        assert!(events.is_empty());
        assert_eq!(methods(&out), ["session/new"]);
        let server = &out[0]["params"]["mcpServers"][0];
        assert_eq!(server["url"], "http://127.0.0.1:9/mcp");
        assert_eq!(server["headers"][0]["value"], "Bearer t");
        assert_eq!(out[0]["params"]["cwd"], "/tmp/w");
    }

    #[test]
    fn 세션이_열리기_전에_보낸_말은_열린_뒤에_나간다() {
        let mut state = state();
        state.waiting_prompt = Some("안녕".into());
        state.model = Some("gpt-6".into());
        let opened = json!({ "jsonrpc": "2.0", "id": 2, "result": {
            "sessionId": "s1",
            "modes": { "currentModeId": "plan" },
            "models": { "currentModelId": "gpt-5", "availableModels": [] },
        } });
        let (out, events) = state.handle(&opened);
        assert!(events.is_empty());
        // 기본 모드로 돌리고, 고른 모델로 바꾸고, 기다리던 말을 보낸다.
        assert_eq!(methods(&out), ["session/set_mode", "session/set_model", "session/prompt"]);
        assert_eq!(out[2]["params"]["prompt"][0]["text"], "안녕");
        assert_eq!(state.session_id.as_deref(), Some("s1"));

        // 그 프롬프트의 응답이 턴의 끝이다.
        let done = json!({ "jsonrpc": "2.0", "id": out[2]["id"], "result": { "stopReason": "end_turn" } });
        assert_eq!(state.handle(&done).1, [AgentEvent::Done]);
    }

    #[test]
    fn 글_조각과_도구_호출을_대화_이벤트로_바꾼다() {
        let mut state = state();
        let update = |update: Value| {
            json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s1", "update": update } })
        };
        let text = update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "확인" } }));
        assert_eq!(state.handle(&text).1, [AgentEvent::Text("확인".into())]);
        let call = update(json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "list_requests" }));
        assert_eq!(
            state.handle(&call).1,
            [AgentEvent::ToolCall("list_requests".into(), Value::Null)]
        );
        // 진행 중 알림은 지나가고, 끝난 알림만 결과가 된다.
        let running = update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "in_progress" }));
        assert!(state.handle(&running).1.is_empty());
        let finished = update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed", "rawOutput": "3" }));
        assert_eq!(
            state.handle(&finished).1,
            [AgentEvent::ToolResult("list_requests".into(), "3".into())]
        );
    }

    #[test]
    fn 권한_요청에는_허용을_고르고_모르는_요청은_거절한다() {
        let mut state = state();
        let ask = json!({ "jsonrpc": "2.0", "id": 7, "method": "session/request_permission", "params": {
            "toolCall": { "title": "list_requests" },
            "options": [
                { "optionId": "allow", "kind": "allow_once" },
                { "optionId": "reject", "kind": "reject_once" },
            ],
        } });
        let (out, _) = state.handle(&ask);
        assert_eq!(out[0]["id"], 7);
        assert_eq!(out[0]["result"]["outcome"]["optionId"], "allow");

        let other = json!({ "jsonrpc": "2.0", "id": 8, "method": "fs/read_text_file", "params": {} });
        let (out, _) = state.handle(&other);
        assert_eq!(out[0]["error"]["code"], -32601);
    }

    #[test]
    fn 프롬프트가_오류로_끝나면_이유를_붙여_실패로_알린다() {
        let mut state = state();
        let prompt = state.prompt("s1", "hi");
        let failed = json!({ "jsonrpc": "2.0", "id": prompt["id"], "error": {
            "code": -32603, "message": "Internal error", "data": "not signed in",
        } });
        assert_eq!(
            state.handle(&failed).1,
            [AgentEvent::Failed("Internal error: not signed in".into())]
        );
        // 같은 말이 두 필드에 다 있으면 한 번만 쓴다.
        let prompt = state.prompt("s1", "hi");
        let repeated = json!({ "jsonrpc": "2.0", "id": prompt["id"], "error": {
            "code": -32000, "message": "Authentication required: sign in again", "data": "sign in again",
        } });
        assert_eq!(
            state.handle(&repeated).1,
            [AgentEvent::Failed("Authentication required: sign in again".into())]
        );
    }
}
