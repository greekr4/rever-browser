//! A chat session with the Claude Code CLI over stream-json: spawn it, send a
//! prompt, stream the reply text back.
//!
//! Follows `src/main/providers/claude-cli-provider.ts`, using the protocol code
//! already ported in rever-agent. Not ported yet: the initialize handshake and
//! model list, set_model, interrupt, and the permission prompt.
//! Until the permission prompt exists nothing can ask the user, so the only tool
//! requests allowed are this app's own MCP tools that the risky-tool rules do not
//! gate: the agent can read the capture and drive the page, but cannot run a shell,
//! touch files, or send attack traffic.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use rever_agent::claude_cli_protocol::{
    MCP_SERVER_NAME, TranslateState, TurnOutcome, claude_cli_args, error_response,
    permission_response, to_permission_request, translate, turn_outcome, user_message,
};
use rever_tool_profile::risky_tools::is_risky_tool_request;
use serde_json::{Value, json};

const SESSION_ID: &str = "rever";

/// True while a user-confirmed Red Team scan runs: the app's own risky MCP
/// tools (probes, mutated replays) auto-approve for its worker turns. The
/// capture-not-send fence stays the safety net — off loopback it still answers
/// mutations with a synthetic 204. Chat turns cannot run concurrently with a
/// scan, so the global cannot leak into an ordinary turn.
pub static REDTEAM_SCAN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[derive(Debug, PartialEq)]
pub enum AgentEvent {
    /// A piece of the reply, in order.
    Text(String),
    /// A tool call started carrying its full input: (name, input).
    ToolCall(String, Value),
    /// A tool call finished: (name, the result text).
    ToolResult(String, String),
    /// The turn finished normally.
    Done,
    /// The turn or the CLI itself failed; the session is no longer usable.
    Failed(String),
}

/// The tool events one ACP update carries: `tool_call` names the call,
/// `tool_call_update` carries its input (in_progress) or output (completed).
/// `names` maps call ids to names across the stream.
fn tool_events(update: &Value, names: &mut std::collections::HashMap<String, String>) -> Vec<AgentEvent> {
    let id = update["toolCallId"].as_str().unwrap_or_default().to_owned();
    match update["sessionUpdate"].as_str() {
        Some("tool_call") => {
            let name = update["title"].as_str().unwrap_or_default().to_owned();
            if !id.is_empty() && !name.is_empty() {
                names.insert(id, name);
            }
            Vec::new()
        }
        Some("tool_call_update") => {
            let name = names.get(&id).cloned().unwrap_or_default();
            if let Some(input) = update.get("rawInput").filter(|value| !value.is_null()) {
                Vec::from([AgentEvent::ToolCall(name, input.clone())])
            } else if let Some(output) = update["rawOutput"].as_str() {
                Vec::from([AgentEvent::ToolResult(name, output.to_owned())])
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// A chat session with one agent, whichever way it is reached: the Claude Code CLI,
/// an ACP binary, or a provider's API. The app pumps [`ChatAgent::drain`] whenever the
/// session woke it.
pub trait ChatAgent {
    /// Hand the agent the user's message; its answer arrives through `drain`.
    fn send(&mut self, text: &str) -> Result<(), String>;
    /// Everything that happened since the last call.
    fn drain(&mut self) -> Vec<AgentEvent>;
    /// Use another model from the next message on, keeping the conversation.
    fn set_model(&mut self, model: &str) -> Result<(), String>;
}

impl ChatAgent for AgentSession {
    fn send(&mut self, text: &str) -> Result<(), String> {
        AgentSession::send(self, text)
            .map_err(|error| format!("Claude Code CLI is not accepting input: {error}"))
    }

    fn drain(&mut self) -> Vec<AgentEvent> {
        AgentSession::drain(self)
    }

    fn set_model(&mut self, model: &str) -> Result<(), String> {
        AgentSession::set_model(self, model).map_err(|error| error.to_string())
    }
}

pub struct AgentSession {
    child: Child,
    stdin: ChildStdin,
    inbox: Arc<Mutex<Vec<Value>>>,
    state: TranslateState,
    /// toolCallId → tool name, so a `tool_call_update` can be attributed.
    tool_names: std::collections::HashMap<String, String>,
}

/// The CLI's arguments. `mcp` is the app's MCP server as (url, Authorization header);
/// when it could not start the config is left out. `--strict-mcp-config` stays either
/// way, so the user's own MCP servers are not loaded.
fn cli_args(mcp: Option<(&str, &str)>, model: Option<&str>) -> Vec<String> {
    if let Some((url, auth_header)) = mcp {
        return claude_cli_args(url, auth_header, model);
    }
    let mut args = claude_cli_args("", "", model);
    if let Some(at) = args.iter().position(|arg| arg == "--mcp-config") {
        args.drain(at..(at + 2).min(args.len()));
    }
    args
}

/// Whether a `can_use_tool` request is answered "allow" without asking — see the
/// module comment. ToolSearch only loads tool schemas.
fn auto_allowed(request: &Value) -> bool {
    let name = request["tool_name"].as_str().unwrap_or_default();
    let own = name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.strip_prefix(MCP_SERVER_NAME))
        .is_some_and(|rest| rest.starts_with("__"));
    let risky = is_risky_tool_request(&to_permission_request(SESSION_ID, request));
    let redteam = REDTEAM_SCAN.load(std::sync::atomic::Ordering::Relaxed);
    name == "ToolSearch" || (own && (!risky || redteam))
}

/// `REVER_CLAUDE_CLI` wins; otherwise `claude` on PATH, then the usual install
/// folders (an app started from the Finder has a minimal PATH).
fn spawn_cli(
    cwd: &Path,
    mcp: Option<(&str, &str)>,
    model: Option<&str>,
) -> Result<Child, String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = match std::env::var("REVER_CLAUDE_CLI") {
        Ok(path) if !path.is_empty() => vec![path],
        _ => vec![
            "claude".to_owned(),
            format!("{home}/.local/bin/claude"),
            "/opt/homebrew/bin/claude".to_owned(),
            "/usr/local/bin/claude".to_owned(),
        ],
    };
    let mut last_error = String::new();
    for candidate in candidates {
        let spawned = Command::new(&candidate)
            .args(cli_args(mcp, model))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match spawned {
            Ok(child) => return Ok(child),
            Err(error) => last_error = format!("{candidate}: {error}"),
        }
    }
    Err(format!(
        "Claude Code CLI could not be started ({last_error})"
    ))
}

/// The stream-json control request that changes the session's model, as one line.
fn set_model_request(model: &str) -> String {
    let request = json!({
        "type": "control_request",
        "request_id": format!("rever-set-model-{model}"),
        "request": { "subtype": "set_model", "model": model },
    });
    format!("{request}\n")
}

impl AgentSession {
    /// `wake` is called from a reader thread whenever [`AgentSession::drain`] has something new.
    /// `model` is the CLI's `--model`; `None` leaves the CLI's own default.
    pub fn spawn(
        cwd: &Path,
        mcp: Option<(&str, &str)>,
        model: Option<&str>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(cwd).map_err(|error| format!("{}: {error}", cwd.display()))?;
        let mut child = spawn_cli(cwd, mcp, model)?;
        let (Some(stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err("Claude Code CLI started without its pipes".to_owned());
        };
        let inbox = Arc::new(Mutex::new(Vec::new()));
        let stderr_text = Arc::new(Mutex::new(String::new()));
        {
            // Drained so a chatty CLI cannot block on a full pipe; kept for the exit message.
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
                // stdout closed: the CLI is gone. Give stderr a moment to be collected.
                std::thread::sleep(std::time::Duration::from_millis(100));
                let detail = stderr_text.lock().unwrap().trim().to_owned();
                inbox
                    .lock()
                    .unwrap()
                    .push(json!({ "type": "rever_exit", "detail": detail }));
                wake();
            });
        }
        Ok(Self {
            child,
            stdin,
            inbox,
            state: TranslateState::default(),
            tool_names: std::collections::HashMap::new(),
        })
    }

    pub fn send(&mut self, text: &str) -> std::io::Result<()> {
        self.stdin.write_all(user_message(text).as_bytes())?;
        self.stdin.flush()
    }

    /// Switch the running session to another model; the conversation is kept.
    pub fn set_model(&mut self, model: &str) -> std::io::Result<()> {
        self.stdin.write_all(set_model_request(model).as_bytes())?;
        self.stdin.flush()
    }

    /// Handle everything the CLI has said since the last call.
    pub fn drain(&mut self) -> Vec<AgentEvent> {
        let messages = std::mem::take(&mut *self.inbox.lock().unwrap());
        let mut events = Vec::new();
        for message in messages {
            match message["type"].as_str() {
                Some("control_request") => {
                    let id = message["request_id"].as_str().unwrap_or_default();
                    let request = &message["request"];
                    let reply = if request["subtype"] == "can_use_tool" {
                        // An answer that is not "allow" is a rejection.
                        let answer = if auto_allowed(request) {
                            json!({ "outcome": { "outcome": "selected", "optionId": "allow" } })
                        } else {
                            Value::Null
                        };
                        permission_response(id, &answer, request.get("input"))
                    } else {
                        error_response(id, "unsupported control request")
                    };
                    let _ = self
                        .stdin
                        .write_all(reply.as_bytes())
                        .and_then(|_| self.stdin.flush());
                }
                Some("result") => events.push(match turn_outcome(&message, false) {
                    TurnOutcome::StopReason(_) => AgentEvent::Done,
                    TurnOutcome::Error(error) => AgentEvent::Failed(error),
                }),
                Some("rever_exit") => {
                    let detail = message["detail"].as_str().unwrap_or_default();
                    let last_line = detail.lines().last().unwrap_or("no output");
                    events.push(AgentEvent::Failed(format!(
                        "Claude Code exited: {last_line}"
                    )));
                }
                _ => {
                    for note in translate(&message, SESSION_ID, &mut self.state) {
                        let update = &note["update"];
                        if update["sessionUpdate"] == "agent_message_chunk" {
                            if let Some(text) = update["content"]["text"].as_str() {
                                events.push(AgentEvent::Text(text.to_owned()));
                            }
                        } else {
                            events.extend(tool_events(update, &mut self.tool_names));
                        }
                    }
                }
            }
        }
        events
    }
}

impl Drop for AgentSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_설정만_빼고_나머지_인자는_그대로_둔다() {
        let args = cli_args(None, None);
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--mcp-config" || arg.contains("mcpServers"))
        );
        assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
        // 빈 값 인자가 제자리에 남아야 한다(뒤 인자를 값으로 먹지 않게).
        let at = args
            .iter()
            .position(|arg| arg == "--setting-sources")
            .unwrap();
        assert_eq!(args[at + 1], "");
        assert_eq!(args[at + 2], "--disallowed-tools");
    }

    #[test]
    fn 고른_모델은_시작_인자와_전환_요청에_들어간다() {
        let args = cli_args(None, Some("claude-opus-5-5"));
        let at = args.iter().position(|arg| arg == "--model").unwrap();
        assert_eq!(args[at + 1], "claude-opus-5-5");
        assert!(!cli_args(None, None).iter().any(|arg| arg == "--model"));

        let line = set_model_request("claude-opus-5-5");
        assert!(line.ends_with('\n'), "한 줄에 한 요청이다");
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["type"], "control_request");
        assert_eq!(request["request"]["subtype"], "set_model");
        assert_eq!(request["request"]["model"], "claude-opus-5-5");
    }

    #[test]
    fn mcp_서버가_있으면_주소와_인증_헤더를_설정에_넣는다() {
        let args = cli_args(Some(("http://127.0.0.1:9/mcp", "Bearer t")), None);
        let at = args.iter().position(|arg| arg == "--mcp-config").unwrap();
        let config: Value = serde_json::from_str(&args[at + 1]).unwrap();
        let server = &config["mcpServers"][MCP_SERVER_NAME];
        assert_eq!(server["url"], "http://127.0.0.1:9/mcp");
        assert_eq!(server["headers"]["Authorization"], "Bearer t");
    }

    fn request(tool: &str, input: Value) -> Value {
        json!({ "subtype": "can_use_tool", "tool_name": tool, "input": input })
    }

    #[test]
    fn 툴_호출은_이름을_등록하고_입력과_출력_이벤트를_만든다() {
        let mut names = std::collections::HashMap::new();
        let named = json!({
            "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "finding_add"
        });
        assert!(tool_events(&named, &mut names).is_empty());
        assert_eq!(names.get("t1").map(String::as_str), Some("finding_add"));

        let input = json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t1",
            "rawInput": { "title": "XSS in q" }, "status": "in_progress"
        });
        assert_eq!(
            tool_events(&input, &mut names),
            vec![AgentEvent::ToolCall("finding_add".into(), json!({ "title": "XSS in q" }))]
        );

        let output = json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t1",
            "rawOutput": "added #7", "status": "completed"
        });
        assert_eq!(
            tool_events(&output, &mut names),
            vec![AgentEvent::ToolResult("finding_add".into(), "added #7".to_owned())]
        );
    }

    #[test]
    fn 등록_안_된_호출_식별자는_빈_이름으로_이벤트를_낸다() {
        let mut names = std::collections::HashMap::new();
        let output = json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "ghost", "rawOutput": "x"
        });
        assert_eq!(
            tool_events(&output, &mut names),
            vec![AgentEvent::ToolResult(String::new(), "x".to_owned())]
        );
    }

    #[test]
    fn 이_앱의_mcp_도구는_묻지_않고_허용한다() {
        assert!(auto_allowed(&request(
            "mcp__rever-traffic__list_requests",
            json!({})
        )));
        assert!(auto_allowed(&request(
            "ToolSearch",
            json!({ "query": "x" })
        )));
    }

    #[test]
    fn 위험_도구와_앱_밖의_도구는_거절한다() {
        // 공격 트래픽을 보내는 도구, 안전하지 않은 메서드의 재전송.
        assert!(!auto_allowed(&request(
            "mcp__rever-traffic__intruder_run",
            json!({ "n": 1 })
        )));
        assert!(!auto_allowed(&request(
            "mcp__rever-traffic__replay_request",
            json!({ "method": "POST" })
        )));
        // 셸, 파일, 다른 MCP 서버, 이름만 비슷한 서버.
        assert!(!auto_allowed(&request("Bash", json!({ "command": "ls" }))));
        assert!(!auto_allowed(&request(
            "Read",
            json!({ "file_path": "/etc/passwd" })
        )));
        assert!(!auto_allowed(&request(
            "mcp__other__list_requests",
            json!({})
        )));
        assert!(!auto_allowed(&request(
            "mcp__rever-traffic-evil__list_requests",
            json!({})
        )));
    }
}
