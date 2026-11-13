//! A chat session with the Claude Code CLI over stream-json: spawn it, send a
//! prompt, stream the reply text back.
//!
//! Follows `src/main/providers/claude-cli-provider.ts`, using the protocol code
//! already ported in rever-agent. Not ported yet: the initialize handshake and
//! model list, set_model, interrupt, the permission prompt, and the MCP tools.
//! Until the permission prompt exists, every tool request from the CLI is rejected,
//! so the agent can talk but cannot run anything on this machine.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use rever_agent::claude_cli_protocol::{
    TranslateState, TurnOutcome, claude_cli_args, error_response, permission_response, translate,
    turn_outcome, user_message,
};
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

/// The CLI's arguments without an MCP server: none exists in this app yet.
/// `--strict-mcp-config` stays, so the user's own MCP servers are not loaded either.
fn cli_args() -> Vec<String> {
    let mut args = claude_cli_args("", "", None);
    if let Some(at) = args.iter().position(|arg| arg == "--mcp-config") {
        args.drain(at..(at + 2).min(args.len()));
    }
    args
}

/// `REVER_CLAUDE_CLI` wins; otherwise `claude` on PATH, then the usual install
/// folders (an app started from the Finder has a minimal PATH).
fn spawn_cli(cwd: &Path) -> Result<Child, String> {
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
            .args(cli_args())
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
    pub fn spawn(cwd: &Path, wake: impl Fn() + Send + Sync + 'static) -> Result<Self, String> {
        std::fs::create_dir_all(cwd).map_err(|error| format!("{}: {error}", cwd.display()))?;
        let mut child = spawn_cli(cwd)?;
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
                    let reply = if message["request"]["subtype"] == "can_use_tool" {
                        // An answer that is not "allow" is a rejection.
                        permission_response(id, &Value::Null, None)
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
        let args = cli_args();
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
}
