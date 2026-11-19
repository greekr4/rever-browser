//! Port of `src/main/model-catalog.ts` — live model lists, the way Orca does it:
//! ask the user's own CLI (no prompt, no tokens), cache the answer on disk so the
//! picker is instant on the next launch, and refresh on each launch so new models
//! show up when the CLI updates. The hardcoded catalog in the UI is only the
//! fallback when a probe has never succeeded.
//!
//! A probe blocks until the CLI answers, so callers run `refresh` off the UI
//! thread; the TypeScript's `models:updated` broadcast is the caller's to make
//! with the catalog `refresh` returns. `prewarmCatalogs` is a loop over
//! [`PROBED_AGENTS`].

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize as _;
use serde_json::{Value, json};

use crate::acp_detect::{AgentProbe, detect_agents};
use crate::agent_env::{AgentEnv, agent_env};
use crate::claude_cli::ClaudeCliFinder;
use crate::grok_login::to_iso_string;
use crate::model_catalog_parse::{
    AgentCatalog, CatalogEntry, CatalogFile, claude_models_from_init, codex_models_from_list,
    parse_catalog_file,
};
use crate::win_shell::{SpawnPlan, shell_safe_spawn};

pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// The agents whose models can be listed.
pub const PROBED_AGENTS: [&str; 2] = ["claude-code", "codex"];

fn command_for(plan: &SpawnPlan) -> Command {
    #[cfg(windows)]
    if plan.shell {
        use std::os::windows::process::CommandExt as _;
        // What Node runs for `shell: true`: the line is already quoted for cmd.exe.
        let mut command = Command::new("cmd.exe");
        command
            .args(["/d", "/s", "/c"])
            .raw_arg(format!("\"{}\"", plan.command));
        return command;
    }
    let mut command = Command::new(&plan.command);
    command.args(&plan.args);
    command
}

/// One JSON-lines conversation with a short-lived CLI process; resolves with
/// whatever `on_line` returns first. `on_line` gets each message and a way to
/// send one back.
fn converse<T>(
    command: &str,
    args: &[&str],
    env: &AgentEnv,
    first: &str,
    timeout: Duration,
    mut on_line: impl FnMut(&Value, &mut dyn FnMut(Value)) -> Result<Option<T>, String>,
) -> Result<T, String> {
    let name = Path::new(command)
        .file_name()
        .map_or_else(|| command.to_owned(), |n| n.to_string_lossy().into_owned());
    let mut child = command_for(&shell_safe_spawn(command, args, cfg!(windows)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear()
        .envs(env)
        .spawn()
        .map_err(|e| e.to_string())?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        return Err(format!("{name} has no pipes"));
    };
    let (lines, incoming) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    // A CLI that is already gone shows up as end of output below.
    let _ = stdin.write_all(first.as_bytes());

    let deadline = Instant::now() + timeout;
    let result = loop {
        match incoming.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(raw) => {
                let raw = raw.trim();
                if raw.is_empty() {
                    continue;
                }
                let message: Value = match serde_json::from_str(raw) {
                    Ok(message) => message,
                    Err(e) => break Err(e.to_string()),
                };
                let mut send = |reply: Value| {
                    let _ = stdin.write_all(format!("{reply}\n").as_bytes());
                };
                match on_line(&message, &mut send) {
                    Ok(Some(out)) => break Ok(out),
                    Ok(None) => {}
                    Err(e) => break Err(e),
                }
            }
            Err(RecvTimeoutError::Timeout) => break Err(format!("{name} model probe timed out")),
            Err(RecvTimeoutError::Disconnected) => {
                let code = child.wait().ok().and_then(|status| status.code());
                let code = code.map_or_else(|| "null".to_owned(), |code| code.to_string());
                break Err(format!("{name} exited ({code}) before listing models"));
            }
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// A JSON value inside a template string: text as it is, anything else as JSON.
fn js_text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn probe_claude(cli: &str, env: &AgentEnv, timeout: Duration) -> Result<Vec<CatalogEntry>, String> {
    let args = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--setting-sources",
        "",
        "--strict-mcp-config",
    ];
    let request = json!({
        "type": "control_request",
        "request_id": "rever-models",
        "request": { "subtype": "initialize" },
    });
    converse(
        cli,
        &args,
        env,
        &format!("{request}\n"),
        timeout,
        |msg, _| {
            let response = &msg["response"];
            if msg["type"] != "control_response" || response["request_id"] != "rever-models" {
                return Ok(None);
            }
            if response["subtype"] == "error" {
                return Err(js_text(&response["error"]));
            }
            Ok(Some(claude_models_from_init(&response["response"])))
        },
    )
}

fn probe_codex(
    cli: &str,
    env: &AgentEnv,
    app_version: &str,
    timeout: Duration,
) -> Result<Vec<CatalogEntry>, String> {
    let init = json!({
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": { "name": "rever-browser", "title": null, "version": app_version },
            "capabilities": null,
        },
    });
    let mut rows = Vec::new();
    converse(
        cli,
        &["app-server"],
        env,
        &format!("{init}\n"),
        timeout,
        |msg, send| {
            if let Some(error) = msg.get("error").filter(|e| !e.is_null() && **e != false) {
                let detail = error
                    .get("message")
                    .filter(|m| !m.is_null())
                    .unwrap_or(error);
                return Err(format!("codex app-server: {}", js_text(detail)));
            }
            let Some(id) = msg["id"].as_f64() else {
                return Ok(None);
            };
            if id == 1.0 {
                send(json!({ "method": "initialized" }));
                send(json!({ "id": 2, "method": "model/list", "params": {} }));
                return Ok(None);
            }
            if id < 2.0 {
                return Ok(None);
            }
            rows.extend(codex_models_from_list(&msg["result"]));
            // shortcut: stops after 5 pages; the list is a handful of models today.
            match msg["result"]["nextCursor"].as_str() {
                Some(next) if !next.is_empty() && id < 6.0 => {
                    send(json!({ // An integer on the wire: `3`, not `3.0`.
                    "id": id as u64 + 1, "method": "model/list", "params": { "cursor": next } }));
                    Ok(None)
                }
                _ => Ok(Some(std::mem::take(&mut rows))),
            }
        },
    )
}

/// The model lists on disk, and the probes that refresh them.
pub struct ModelCatalog {
    cache_file: PathBuf,
    cache: Mutex<Option<CatalogFile>>,
    // One cell per running probe: callers that arrive meanwhile wait on it.
    inflight: Mutex<HashMap<String, Arc<OnceLock<Option<AgentCatalog>>>>>,
}

impl ModelCatalog {
    /// `user_data_dir` is the app's data folder; the cache is `model-catalog.json` in it.
    pub fn new(user_data_dir: &Path) -> Self {
        Self {
            cache_file: user_data_dir.join("model-catalog.json"),
            cache: Mutex::new(None),
            inflight: Mutex::new(HashMap::new()),
        }
    }

    fn with_file<T>(&self, f: impl FnOnce(&mut CatalogFile) -> T) -> T {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        f(cache.get_or_insert_with(|| {
            // A missing file parses like an unrecognised one: an empty catalog.
            parse_catalog_file(&std::fs::read_to_string(&self.cache_file).unwrap_or_default())
        }))
    }

    pub fn cached(&self, agent_id: &str) -> Option<AgentCatalog> {
        self.with_file(|file| file.agents.get(agent_id).cloned())
    }

    /// Re-list one agent's models; concurrent calls share the probe. A failed
    /// probe keeps the old cache and returns it. `now_ms` is read when the probe
    /// has answered.
    pub fn refresh(
        &self,
        agent_id: &str,
        claude_cli: &ClaudeCliFinder,
        app_version: &str,
        now_ms: impl FnOnce() -> i64,
    ) -> Option<AgentCatalog> {
        let probe = || match agent_id {
            "claude-code" => {
                let cli = claude_cli.find().ok_or("claude CLI not found")?;
                probe_claude(cli, &agent_env(cli, claude_cli), PROBE_TIMEOUT)
            }
            _ => {
                let found = detect_agents(&[AgentProbe {
                    command: "codex".to_owned(),
                    fallback_bins: None,
                }]);
                let cli = found
                    .first()
                    .and_then(|found| found.resolved_path.as_deref())
                    .ok_or("codex CLI not found")?;
                probe_codex(cli, &agent_env(cli, claude_cli), app_version, PROBE_TIMEOUT)
            }
        };
        if !PROBED_AGENTS.contains(&agent_id) {
            return None;
        }
        self.refresh_with(agent_id, probe, now_ms)
    }

    fn refresh_with(
        &self,
        agent_id: &str,
        probe: impl FnOnce() -> Result<Vec<CatalogEntry>, String>,
        now_ms: impl FnOnce() -> i64,
    ) -> Option<AgentCatalog> {
        let cell = {
            let mut inflight = self.inflight.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(inflight.entry(agent_id.to_owned()).or_default())
        };
        cell.get_or_init(|| {
            let fresh = probe()
                .and_then(|models| match models.is_empty() {
                    true => Err("empty model list".to_owned()),
                    false => Ok(models),
                })
                .map(|models| self.store(agent_id, models, now_ms()));
            let result = match fresh {
                Ok(entry) => Some(entry),
                Err(e) => {
                    eprintln!("[models] {agent_id} probe failed: {e}");
                    self.cached(agent_id)
                }
            };
            let mut inflight = self.inflight.lock().unwrap_or_else(PoisonError::into_inner);
            inflight.remove(agent_id);
            result
        })
        .clone()
    }

    fn store(&self, agent_id: &str, models: Vec<CatalogEntry>, now_ms: i64) -> AgentCatalog {
        let entry = AgentCatalog {
            models,
            fetched_at: to_iso_string(now_ms).unwrap_or_default(),
        };
        self.with_file(|file| {
            file.agents.insert(agent_id.to_owned(), entry.clone());
            // `JSON.stringify(file, null, 1)`.
            let mut text = Vec::new();
            let indent = serde_json::ser::PrettyFormatter::with_indent(b" ");
            let written = file
                .serialize(&mut serde_json::Serializer::with_formatter(
                    &mut text, indent,
                ))
                .map_err(|e| e.to_string())
                .and_then(|()| std::fs::write(&self.cache_file, text).map_err(|e| e.to_string()));
            if let Err(e) = written {
                eprintln!("[models] could not write cache: {e}");
            }
        });
        entry
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    const QUICK: Duration = Duration::from_secs(10);

    /// A fake CLI: a shell script that talks JSON lines on its pipes.
    fn script(dir: &TempDir, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn env() -> AgentEnv {
        AgentEnv::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())])
    }

    fn entry(id: &str) -> CatalogEntry {
        CatalogEntry {
            model_id: id.to_owned(),
            name: id.to_owned(),
            description: None,
            effort_levels: None,
            supports_fast_mode: None,
        }
    }

    #[test]
    fn 클로드_cli의_initialize_응답에서_모델을_읽는다() {
        let dir = TempDir::new("models-claude");
        let cli = script(
            &dir,
            "claude",
            r#"read line
case "$line" in *'"request_id":"rever-models"'*) ;; *) exit 9 ;; esac
echo
echo '{"type":"system","subtype":"init"}'
echo '{"type":"control_response","response":{"request_id":"other","response":{"models":[{"value":"no"}]}}}'
echo '{"type":"control_response","response":{"request_id":"rever-models","subtype":"success","response":{"models":[{"value":"opus","displayName":"Opus","supportedEffortLevels":["low","high"],"supportsFastMode":true}]}}}'
sleep 5"#,
        );
        let models = probe_claude(&cli, &env(), QUICK).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            (models[0].model_id.as_str(), models[0].name.as_str()),
            ("opus", "Opus")
        );
        assert_eq!(models[0].supports_fast_mode, Some(true));
    }

    #[test]
    fn 클로드_cli가_오류로_답하면_그_문구로_실패한다() {
        let dir = TempDir::new("models-claude-error");
        let cli = script(
            &dir,
            "claude",
            r#"read line
echo '{"type":"control_response","response":{"request_id":"rever-models","subtype":"error","error":"not logged in"}}'"#,
        );
        assert_eq!(
            probe_claude(&cli, &env(), QUICK),
            Err("not logged in".to_owned())
        );
    }

    #[test]
    fn 코덱스는_커서를_따라_여러_쪽을_모은다() {
        let dir = TempDir::new("models-codex");
        let cli = script(
            &dir,
            "codex",
            r#"[ "$1" = app-server ] || exit 9
read init
case "$init" in *'"version":"1.2.3"'*) ;; *) exit 8 ;; esac
echo '{"id":1,"result":{}}'
read initialized
read first
echo '{"method":"note"}'
echo '{"id":2,"result":{"data":[{"id":"b"},{"id":"a","isDefault":true},{"id":"h","hidden":true}],"nextCursor":"c2"}}'
read second
case "$second" in *'"id":3,'*'"cursor":"c2"'*) ;; *) exit 7 ;; esac
echo '{"id":3,"result":{"data":[{"id":"z","displayName":"Zed"}]}}'
sleep 5"#,
        );
        let models = probe_codex(&cli, &env(), "1.2.3", QUICK).unwrap();
        let ids: Vec<&str> = models.iter().map(|m| m.model_id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "z"]);
        assert_eq!(models[2].name, "Zed");
    }

    #[test]
    fn 코덱스가_오류를_보내면_그_메시지로_실패한다() {
        let dir = TempDir::new("models-codex-error");
        let cli = script(
            &dir,
            "codex",
            r#"read init
echo '{"id":1,"error":{"code":-1,"message":"bad client"}}'"#,
        );
        assert_eq!(
            probe_codex(&cli, &env(), "1", QUICK),
            Err("codex app-server: bad client".to_owned())
        );
    }

    #[test]
    fn 목록을_내기_전에_끝나거나_답이_없으면_실패한다() {
        let dir = TempDir::new("models-exit");
        let quits = script(&dir, "quits", "exit 3");
        assert_eq!(
            probe_claude(&quits, &env(), QUICK),
            Err("quits exited (3) before listing models".to_owned())
        );
        let silent = script(&dir, "silent", "sleep 5");
        assert_eq!(
            probe_claude(&silent, &env(), Duration::from_millis(100)),
            Err("silent model probe timed out".to_owned())
        );
        let garbage = script(&dir, "garbage", "echo not-json; sleep 5");
        assert!(probe_claude(&garbage, &env(), QUICK).is_err());
        assert!(probe_claude("/nonexistent/claude", &env(), QUICK).is_err());
    }

    #[test]
    fn 성공한_목록은_캐시_파일에_남고_다음_실행에서_읽힌다() {
        let dir = TempDir::new("models-cache");
        let catalog = ModelCatalog::new(dir.path());
        assert_eq!(catalog.cached("codex"), None);
        let fresh = catalog.refresh_with("codex", || Ok(vec![entry("a")]), || 1_791_421_323_000);
        let expected = AgentCatalog {
            models: vec![entry("a")],
            fetched_at: "2026-10-08T01:02:03.000Z".to_owned(),
        };
        assert_eq!(fresh, Some(expected.clone()));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("model-catalog.json")).unwrap(),
            "{\n \"version\": 1,\n \"agents\": {\n  \"codex\": {\n   \"models\": [\n    {\n     \"modelId\": \"a\",\n     \"name\": \"a\",\n     \"description\": null\n    }\n   ],\n   \"fetchedAt\": \"2026-10-08T01:02:03.000Z\"\n  }\n }\n}"
        );
        assert_eq!(
            ModelCatalog::new(dir.path()).cached("codex"),
            Some(expected)
        );
    }

    #[test]
    fn 실패하거나_빈_목록이면_이전_캐시를_지킨다() {
        let dir = TempDir::new("models-keep");
        let catalog = ModelCatalog::new(dir.path());
        let first = catalog.refresh_with("codex", || Ok(vec![entry("a")]), || 0);
        assert_eq!(
            catalog.refresh_with("codex", || Err("down".to_owned()), || 5),
            first
        );
        assert_eq!(
            catalog.refresh_with("codex", || Ok(Vec::new()), || 5),
            first
        );
        assert_eq!(
            catalog.refresh_with("claude-code", || Err("down".to_owned()), || 5),
            None
        );
        // The probe ran each time: a finished refresh does not pin its answer.
        let again = catalog.refresh_with("codex", || Ok(vec![entry("b")]), || 5);
        assert_eq!(again.map(|c| c.models), Some(vec![entry("b")]));
    }

    #[test]
    fn 모르는_에이전트는_탐색하지_않고_깨진_캐시는_빈_것으로_본다() {
        let dir = TempDir::new("models-unknown");
        std::fs::write(dir.path().join("model-catalog.json"), "{broken").unwrap();
        let catalog = ModelCatalog::new(dir.path());
        assert_eq!(
            catalog.refresh("glm", &ClaudeCliFinder::new(), "1", || 0),
            None
        );
        assert_eq!(catalog.cached("codex"), None);
    }
}
