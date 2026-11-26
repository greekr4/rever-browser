//! A chat session with the Claude Code CLI over stream-json: spawn it, send a
//! prompt, stream the reply text back.
//!
//! Follows `src/main/providers/claude-cli-provider.ts`, using the protocol code
//! already ported in rever-agent. Not ported yet: the initialize handshake and
//! model list, set_model, interrupt, and the permission prompt.
//! The app's tools are the `revb` CLI, so the only Bash requests auto-approved are
//! a single `revb` command the risky-tool rules do not gate: the agent can read the
//! capture and drive the page, but anything else — a second command, a pipe, attack
//! traffic — is asked about or refused.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

use rever_agent::claude_cli_protocol::{
    TranslateState, TurnOutcome, claude_cli_args, error_response, permission_response,
    to_permission_request, translate, turn_outcome, user_message,
};
use rever_chat_core::acp_permission::RequestPermissionResponse;
use rever_tool_profile::risky_tools::is_own_cli;
use rever_tool_profile::risky_tools::is_risky_tool_request;
use serde_json::{Value, json};

use rever_core::paths::user_data_dir;

pub(crate) const SESSION_ID: &str = "rever";

/// The agent asking before a tool runs. Whoever takes the event answers it: the chat
/// puts it to the user, a scan refuses it.
pub struct PermissionAsk {
    /// The ACP `RequestPermissionRequest`.
    pub request: Value,
    /// Hands the answer back to the agent.
    pub answer: Box<dyn FnOnce(RequestPermissionResponse) + Send>,
}

impl std::fmt::Debug for PermissionAsk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionAsk")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

// Two asks are the same when they ask the same thing; where the answer goes is not
// something to compare.
impl PartialEq for PermissionAsk {
    fn eq(&self, other: &Self) -> bool {
        self.request == other.request
    }
}

/// Lines for an agent's stdin that are ready only after its `drain` returned: the
/// answer to a permission the user took a while to give. The next `drain` writes them.
#[derive(Clone)]
pub(crate) struct Outbox {
    lines: Arc<Mutex<Vec<String>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Outbox {
    pub(crate) fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            lines: Arc::default(),
            wake,
        }
    }

    /// Queue `line` and wake the app, which drains the session.
    pub(crate) fn push(&self, line: String) {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line);
        (self.wake)();
    }

    pub(crate) fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.lines.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// True while a user-confirmed Red Team scan runs: the app's own risky MCP
/// tools (probes, mutated replays) auto-approve for its worker turns. The
/// capture-not-send fence stays the safety net — off loopback it still answers
/// mutations with a synthetic 204. Chat turns cannot run concurrently with a
/// scan, so the global cannot leak into an ordinary turn.
pub static REDTEAM_SCAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
    /// The agent waits for a yes or no before it runs a tool.
    Permission(PermissionAsk),
}

/// The tool events one ACP update carries: `tool_call` names the call,
/// `tool_call_update` carries its input (in_progress) or output (completed).
/// `names` maps call ids to names across the stream.
fn tool_events(
    update: &Value,
    names: &mut std::collections::HashMap<String, String>,
) -> Vec<AgentEvent> {
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
// `Send`: a workflow drives its agent from the executor's thread.
pub trait ChatAgent: Send {
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
    outbox: Outbox,
}

/// The CLI's arguments. The app's tools are a CLI (`revb`), so no MCP server is
/// configured; `--strict-mcp-config` still keeps the user's own out.
fn cli_args(model: Option<&str>) -> Vec<String> {
    claude_cli_args(model)
}

/// Whether a `can_use_tool` request is answered "allow" without asking — see the
/// module comment. ToolSearch only loads tool schemas.
fn auto_allowed(request: &Value) -> bool {
    let name = request["tool_name"].as_str().unwrap_or_default();
    if name == "ToolSearch" {
        return true;
    }
    // The app's tools run through `revb` in a shell; every other command is the
    // user's to approve.
    if name != "Bash" {
        return false;
    }
    let command = request
        .pointer("/input/command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_own_cli(command) {
        return false;
    }
    let risky = is_risky_tool_request(&to_permission_request(SESSION_ID, request));
    let redteam = REDTEAM_SCAN.load(std::sync::atomic::Ordering::Relaxed);
    !risky || redteam
}

/// The `revb` script next to `exe`: a dev checkout is `target/debug/rever` or
/// `target/release/rever` (three levels up), a packaged layout may be flatter.
/// Walk up at most six levels looking for `<root>/bin/revb.mjs`.
fn revb_script_from(exe: &Path) -> Option<PathBuf> {
    let mut dir = exe.parent();
    for _ in 0..6 {
        let root = dir?;
        let script = root.join("bin").join("revb.mjs");
        if script.is_file() {
            return Some(script);
        }
        dir = root.parent();
    }
    None
}

/// The `revb` script the child CLI will call: `REVER_REVB_DIR` wins, otherwise
/// the checkout next to the running binary.
fn revb_script() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("REVER_REVB_DIR") {
        let script = PathBuf::from(dir).join("revb.mjs");
        return script.is_file().then_some(script);
    }
    revb_script_from(&std::env::current_exe().ok()?)
}

/// Write a `revb` shim that runs `script` into `<data_dir>/bin`, returning the
/// directory to put first on a child's PATH. The shim is what lets the agent's
/// CLI reach the tools with no install step — the app's own PATH is minimal.
fn write_revb_shim(script: &Path, data_dir: &Path) -> Option<PathBuf> {
    let dir = data_dir.join("bin");
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let shim = dir.join("revb");
    // A single quote inside the path would end the sh's string early.
    let escaped = script.display().to_string().replace('\'', "'\\''");
    let body = format!("#!/bin/sh\nexec node '{escaped}' \"$@\"\n");
    let stale = std::fs::read_to_string(&shim).map_or(true, |old| old != body);
    if stale {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::write(&shim, &body).is_ok() {
                let _ = std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
    Some(dir)
}

/// The shim directory first on the child's PATH, so the agent's shell finds it.
fn prepend_to_path(cmd: &mut Command, script: &Path, data_dir: &Path) -> Option<PathBuf> {
    let dir = write_revb_shim(script, data_dir)?;
    let mut paths = vec![dir.clone()];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    cmd.env("PATH", std::env::join_paths(paths).unwrap_or_default());
    Some(dir)
}

/// Resolve the script and the data dir from the environment, then prepend.
fn prepend_revb_path(cmd: &mut Command) {
    let Some(script) = revb_script() else {
        eprintln!(
            "[agent] revb.mjs was not found; the agent cannot call tools. Set REVER_REVB_DIR to its folder."
        );
        return;
    };
    let data = user_data_dir(
        |name| std::env::var(name).ok(),
        std::env::consts::OS,
        &std::env::var("HOME").unwrap_or_default(),
    );
    prepend_to_path(cmd, &script, &data);
}

/// `REVER_CLAUDE_CLI` wins; otherwise `claude` on PATH, then the usual install
/// folders (an app started from the Finder has a minimal PATH).
fn spawn_cli(cwd: &Path, _mcp: Option<(&str, &str)>, model: Option<&str>) -> Result<Child, String> {
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
        let mut cmd = Command::new(&candidate);
        cmd.args(cli_args(model))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        prepend_revb_path(&mut cmd);
        match cmd.spawn() {
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
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let outbox = Outbox::new(wake.clone());
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
            outbox,
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
        // Answers the user gave since the last call.
        for line in self.outbox.take() {
            let _ = self.stdin.write_all(line.as_bytes());
        }
        let _ = self.stdin.flush();
        let messages = std::mem::take(&mut *self.inbox.lock().unwrap());
        let mut events = Vec::new();
        for message in messages {
            match message["type"].as_str() {
                Some("control_request") => {
                    let id = message["request_id"].as_str().unwrap_or_default();
                    let request = &message["request"];
                    let reply = if request["subtype"] != "can_use_tool" {
                        error_response(id, "unsupported control request")
                    } else if auto_allowed(request) {
                        let allow =
                            json!({ "outcome": { "outcome": "selected", "optionId": "allow" } });
                        permission_response(id, &allow, request.get("input"))
                    } else {
                        // Someone has to say yes: the answer comes through the outbox.
                        let (outbox, id) = (self.outbox.clone(), id.to_owned());
                        let input = request.get("input").cloned();
                        events.push(AgentEvent::Permission(PermissionAsk {
                            request: to_permission_request(SESSION_ID, request),
                            answer: Box::new(move |answer| {
                                let answer = serde_json::to_value(answer).unwrap_or_default();
                                outbox.push(permission_response(&id, &answer, input.as_ref()));
                            }),
                        }));
                        continue;
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
    fn mcp_서버는_하나도_붙이지_않는다() {
        let args = cli_args(None);
        let at = args.iter().position(|arg| arg == "--mcp-config").unwrap();
        let config: Value = serde_json::from_str(&args[at + 1]).unwrap();
        assert_eq!(config["mcpServers"], json!({}));
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
        let args = cli_args(Some("claude-opus-5-5"));
        let at = args.iter().position(|arg| arg == "--model").unwrap();
        assert_eq!(args[at + 1], "claude-opus-5-5");
        assert!(!cli_args(None).iter().any(|arg| arg == "--model"));

        let line = set_model_request("claude-opus-5-5");
        assert!(line.ends_with('\n'), "한 줄에 한 요청이다");
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["type"], "control_request");
        assert_eq!(request["request"]["subtype"], "set_model");
        assert_eq!(request["request"]["model"], "claude-opus-5-5");
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
            vec![AgentEvent::ToolCall(
                "finding_add".into(),
                json!({ "title": "XSS in q" })
            )]
        );

        let output = json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t1",
            "rawOutput": "added #7", "status": "completed"
        });
        assert_eq!(
            tool_events(&output, &mut names),
            vec![AgentEvent::ToolResult(
                "finding_add".into(),
                "added #7".to_owned()
            )]
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
    fn 저장소_뿌리의_revb_mjs를_실행파일_위에서_찾는다() {
        let dir = std::env::temp_dir().join(format!("rever-script-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let script = dir.join("bin/revb.mjs");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "// script\n").unwrap();
        let exe = dir.join("target/debug/rever");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "").unwrap();
        assert_eq!(revb_script_from(&exe), Some(script.clone()));
        // release 레이아웃도 같은 뿌리를 가리킨다.
        let exe = dir.join("target/release/rever");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "").unwrap();
        assert_eq!(revb_script_from(&exe).unwrap().to_string_lossy(), script.to_string_lossy());
        // 스크립트가 없으면 아무것도 아니다. 같은 뿌리에 스크립트가 있으면
        // 안 되므로 별도 폴더에서 확인한다.
        let bare_root = std::env::temp_dir().join(format!("rever-script-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&bare_root);
        let bare = bare_root.join("other/deep/nest/rever");
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        std::fs::write(&bare, "").unwrap();
        assert!(revb_script_from(&bare).is_none());
        let _ = std::fs::remove_dir_all(&bare_root);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 에이전트_경로에_revb_심이_먼저_들어간다() {
        let dir = std::env::temp_dir().join(format!("rever-revb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        let script = dir.join("scripts/revb.mjs");
        std::fs::write(&script, "// shim target\n").unwrap();
        let mut cmd = Command::new("true");
        let shim_dir = prepend_to_path(&mut cmd, &script, &dir.join("data")).unwrap();
        let envs: Vec<_> = cmd.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))).collect();
        let path = envs
            .iter()
            .find(|(k, _)| *k == "PATH")
            .and_then(|(_, v)| v.to_str())
            .unwrap();
        assert!(path.starts_with(shim_dir.to_str().unwrap()));
        let shim = dir.join("data/bin/revb");
        assert!(shim.is_file());
        let body = std::fs::read_to_string(&shim).unwrap();
        assert!(body.contains("revb.mjs"), "{body}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn revb_한_번짜리_명령은_묻지_않고_허용한다() {
        assert!(auto_allowed(&request(
            "Bash",
            json!({ "command": "revb call list_requests '{}'" })
        )));
        assert!(auto_allowed(&request(
            "ToolSearch",
            json!({ "query": "x" })
        )));
    }

    #[test]
    fn 위험_도구와_앱_밖의_명령은_거절한다() {
        // 공격 트래픽을 보내는 도구, 안전하지 않은 메서드의 재전송.
        assert!(!auto_allowed(&request(
            "Bash",
            json!({ "command": "revb call intruder_run '{}'" })
        )));
        assert!(!auto_allowed(&request(
            "Bash",
            json!({ "command": "revb call replay_request '{\"method\":\"POST\"}'" })
        )));
        // 셸, 파일, 두 번째 명령.
        assert!(!auto_allowed(&request("Bash", json!({ "command": "ls" }))));
        assert!(!auto_allowed(&request(
            "Bash",
            json!({ "command": "revb list; rm -rf ~" })
        )));
        assert!(!auto_allowed(&request(
            "Read",
            json!({ "file_path": "/etc/passwd" })
        )));
    }
}
