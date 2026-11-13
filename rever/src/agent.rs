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

pub enum AgentEvent {
    /// A piece of the reply, in order.
    Text(String),
    /// The turn finished normally.
    Done,
    /// The turn or the CLI itself failed; the session is no longer usable.
    Failed(String),
}

pub struct AgentSession {
    child: Child,
    stdin: ChildStdin,
    inbox: Arc<Mutex<Vec<Value>>>,
    state: TranslateState,
}

/// The CLI's arguments. `mcp` is the app's MCP server as (url, Authorization header);
/// when it could not start the config is left out. `--strict-mcp-config` stays either
/// way, so the user's own MCP servers are not loaded.
fn cli_args(mcp: Option<(&str, &str)>) -> Vec<String> {
    if let Some((url, auth_header)) = mcp {
        return claude_cli_args(url, auth_header, None);
    }
    let mut args = claude_cli_args("", "", None);
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
    name == "ToolSearch"
        || (own && !is_risky_tool_request(&to_permission_request(SESSION_ID, request)))
}

/// `REVER_CLAUDE_CLI` wins; otherwise `claude` on PATH, then the usual install
/// folders (an app started from the Finder has a minimal PATH).
fn spawn_cli(cwd: &Path, mcp: Option<(&str, &str)>) -> Result<Child, String> {
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
            .args(cli_args(mcp))
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

impl AgentSession {
    /// `wake` is called from a reader thread whenever [`AgentSession::drain`] has something new.
    pub fn spawn(
        cwd: &Path,
        mcp: Option<(&str, &str)>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(cwd).map_err(|error| format!("{}: {error}", cwd.display()))?;
        let mut child = spawn_cli(cwd, mcp)?;
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
        })
    }

    pub fn send(&mut self, text: &str) -> std::io::Result<()> {
        self.stdin.write_all(user_message(text).as_bytes())?;
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
        let args = cli_args(None);
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
    fn mcp_서버가_있으면_주소와_인증_헤더를_설정에_넣는다() {
        let args = cli_args(Some(("http://127.0.0.1:9/mcp", "Bearer t")));
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
