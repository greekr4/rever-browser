//! Scoreboard for the port from TypeScript/Electron to Rust/GPUI.
//!
//!   cargo run -p port-harness            # full run: inventory, coverage, behavior checks
//!   cargo run -p port-harness -- --no-run  # inventory and coverage only (no build, no app launch)
//!   cargo run -p port-harness -- --app-only  # skip the per-crate unit tests (other work is mid-edit)
//!
//! Coverage is read from the sources, not from a hand-kept list:
//!   * a Rust file counts as the port of a TypeScript file when it says
//!     "Port of `src/...`" in a comment;
//!   * an MCP tool counts as ported when a Rust file registers it as `ToolDef::new("name"`.
//!
//! Behavior checks launch the real app against a fixture server started here.
//! The result is also written to target/port-status.json. Exit code 1 if a check fails.

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

/// Stands in for the Claude Code CLI: streams a reply, asks to run a tool, and reports
/// whether the app allowed or rejected it. No model is called.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
answers=0
while IFS= read -r line; do
  case "$line" in
    *'"type":"user"'*)
      printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"fake "}}}'
      printf '%s\n' '{"type":"control_request","request_id":"p1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}'
      ;;
    *'"type":"control_response"'*)
      case "$line" in *'"behavior":"deny"'*) verdict=rejected ;; *) verdict=allowed ;; esac
      answers=$((answers + 1))
      if [ "$answers" -eq 1 ]; then
        shell=$verdict
        printf '%s\n' '{"type":"control_request","request_id":"p2","request":{"subtype":"can_use_tool","tool_name":"mcp__rever-traffic__list_requests","input":{}}}'
      else
        printf '%s\n' "{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"reply, shell $shell, own tool $verdict\"}}}"
        printf '%s\n' '{"type":"result","subtype":"success","stop_reason":"end_turn"}'
      fi
      ;;
  esac
done
"#;

const FIXTURE_PAGE: &str = "<!doctype html><title>port harness fixture</title><h1>port harness fixture</h1>\
<p style=\"color:#b00;background:#eef\">colored text so the frame is not one flat color</p>\
<script>localStorage.setItem('harness-key', 'harness-value');fetch('/api/items').then(function (r) { return r.json() });\
var x = new XMLHttpRequest(); x.open('POST', '/api/login'); x.send('user=rever&pass=servo')</script>";

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf();
    let crates_dir = root.join("crates");
    let run_checks = !std::env::args().any(|arg| arg == "--no-run");

    let ts_files = files_under(&root.join("src"), &["ts", "tsx"]);
    // Every `rever*` directory under crates/ is a crate of the port.
    let mut crates: Vec<String> = fs::read_dir(&crates_dir)
        .map(|dir| {
            dir.flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    crates.retain(|name| {
        name.starts_with("rever") && crates_dir.join(name).join("Cargo.toml").is_file()
    });
    crates.sort();
    let rust_files: Vec<PathBuf> = crates
        .iter()
        .flat_map(|krate| files_under(&crates_dir.join(krate).join("src"), &["rs"]))
        .collect();
    let rust_sources: Vec<String> = rust_files
        .iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .collect();

    // ── Inventory: what the TypeScript app has ──────────────────────────────
    let ts: Vec<(String, String)> = ts_files
        .iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(&root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some((relative, fs::read_to_string(path).ok()?))
        })
        .filter(|(path, _)| !path.contains(".test.") && !path.ends_with(".d.ts"))
        .collect();
    let lines = |prefix: &str| -> usize {
        ts.iter()
            .filter(|(path, _)| path.starts_with(prefix))
            .map(|(_, text)| text.lines().count())
            .sum()
    };
    let count = |prefix: &str| {
        ts.iter()
            .filter(|(path, _)| path.starts_with(prefix))
            .count()
    };
    let tools: BTreeSet<String> = ts
        .iter()
        .filter(|(path, _)| path.starts_with("src/main/mcp/"))
        .flat_map(|(_, text)| string_after(text, "registerTool("))
        .collect();

    // ── Coverage: what the Rust port has ────────────────────────────────────
    let ported_files: BTreeSet<String> = rust_sources
        .iter()
        .flat_map(|text| string_after(text, "Port of "))
        // Only markers that name a real source file count.
        .filter(|path| ts.iter().any(|(ts_path, _)| ts_path == path))
        .collect();
    let ported_tools: BTreeSet<String> = rust_sources
        .iter()
        .flat_map(|text| string_after(text, "ToolDef::new("))
        .filter(|name| tools.contains(name))
        .collect();
    let ported_lines: usize = ts
        .iter()
        .filter(|(path, _)| ported_files.contains(path))
        .map(|(_, text)| text.lines().count())
        .sum();
    let rust_lines: usize = rust_sources.iter().map(|text| text.lines().count()).sum();
    let patches = fs::read_dir(root.join("patches"))
        .map(|dir| dir.flatten().count())
        .unwrap_or(0);

    if ts.is_empty() {
        println!(
            "== Inventory (TypeScript/Electron app) ==\n  (removed — the Rust port is the app; history and main keep the original)"
        );
    } else {
        println!("== Inventory (TypeScript/Electron app) ==");
    }
    println!(
        "  main process    {:>4} files  {:>6} lines",
        count("src/main/"),
        lines("src/main/")
    );
    println!(
        "  interface       {:>4} files  {:>6} lines",
        count("src/renderer/"),
        lines("src/renderer/")
    );
    println!(
        "  preload/shared  {:>4} files  {:>6} lines",
        count("src/preload/") + count("src/shared/"),
        lines("src/preload/") + lines("src/shared/")
    );
    println!("  MCP tools       {:>4}", tools.len());
    println!("== Coverage (Rust port) ==");
    println!(
        "  Rust written    {:>4} files  {:>6} lines",
        rust_files.len(),
        rust_lines
    );
    println!(
        "  TS files ported {:>4} / {}   ({} of {} lines)",
        ported_files.len(),
        ts.len(),
        ported_lines,
        lines("src/")
    );
    for file in &ported_files {
        println!("      {file}");
    }
    println!(
        "  MCP tools       {:>4} / {}",
        ported_tools.len(),
        tools.len()
    );
    println!("  crate patches   {:>4}", patches);

    // ── Behavior: does the Rust app do the thing ────────────────────────────
    let mut checks: Vec<(String, bool, String)> = Vec::new();
    if run_checks {
        println!("== Behavior checks ==");
        let cargo = |args: &[&str]| -> (bool, String) {
            match Command::new("cargo").args(args).current_dir(&root).output() {
                Ok(out) => {
                    let text = String::from_utf8_lossy(&out.stderr).into_owned()
                        + &String::from_utf8_lossy(&out.stdout);
                    // The first "test result" line is the unit tests; later ones are doc-tests.
                    let last = text
                        .lines()
                        .find(|line| line.contains("test result") || line.starts_with("error"))
                        .unwrap_or("")
                        .trim()
                        .to_owned();
                    (out.status.success(), last)
                }
                Err(error) => (false, error.to_string()),
            }
        };
        let app_only = std::env::args().any(|arg| arg == "--app-only");
        for krate in crates.iter().filter(|_| !app_only) {
            let (ok, detail) = cargo(&["test", "-p", krate.as_str()]);
            checks.push((format!("{krate} unit tests pass"), ok, detail));
        }
        let (ok, detail) = cargo(&["build", "-p", "rever"]);
        checks.push(("app builds (GPUI + Servo in one binary)".into(), ok, detail));
        if ok {
            checks.extend(app_checks(&root));
        }
        for (name, ok, detail) in &checks {
            println!(
                "  [{}] {name}{}",
                if *ok { "PASS" } else { "FAIL" },
                if detail.is_empty() {
                    String::new()
                } else {
                    format!("  — {detail}")
                }
            );
        }
    }

    let failed = checks.iter().filter(|(_, ok, _)| !ok).count();
    let status = json!({
        "inventory": {
            "mainFiles": count("src/main/"), "mainLines": lines("src/main/"),
            "interfaceFiles": count("src/renderer/"), "interfaceLines": lines("src/renderer/"),
            "tsFiles": ts.len(), "tsLines": lines("src/"), "mcpTools": tools.len(),
        },
        "coverage": {
            "rustFiles": rust_files.len(), "rustLines": rust_lines,
            "tsFilesPorted": ported_files, "tsLinesPorted": ported_lines,
            "mcpToolsPorted": ported_tools, "cratePatches": patches,
        },
        "checks": checks.iter().map(|(name, ok, detail)| json!({ "name": name, "pass": ok, "detail": detail })).collect::<Vec<_>>(),
    });
    let _ = fs::create_dir_all(root.join("target"));
    let _ = fs::write(
        root.join("target/port-status.json"),
        serde_json::to_string_pretty(&status).unwrap_or_default(),
    );
    if run_checks {
        println!(
            "== {} of {} checks passed ==",
            checks.len() - failed,
            checks.len()
        );
    }
    std::process::exit(if failed == 0 { 0 } else { 1 });
}

/// Launch the app on a fixture page and check what it rendered and captured.
fn app_checks(root: &Path) -> Vec<(String, bool, String)> {
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture port");
    let port = listener.local_addr().expect("fixture address").port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buffer = vec![0u8; 16 * 1024];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let posted = request.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
            let (mime, body) = match path.as_str() {
                "/api/items" => ("application/json", "{\"items\":[1,2,3]}".to_owned()),
                "/api/login" => ("application/json", json!({ "echoed": posted }).to_string()),
                _ => ("text/html", FIXTURE_PAGE.to_owned()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });

    let out_dir = root.join("target/port-harness");
    let _ = fs::remove_dir_all(&out_dir);
    let _ = fs::create_dir_all(&out_dir);
    let fake_claude = out_dir.join("fake-claude.sh");
    let _ = fs::write(&fake_claude, FAKE_CLAUDE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755));
    }
    let user_data = out_dir.join("user-data");
    let child = Command::new(root.join("target/debug/rever"))
        .env("REVER_CLAUDE_CLI", &fake_claude)
        .env("REVER_USER_DATA_DIR", &user_data)
        .arg("--say=hello")
        .arg(format!("--self-check={}", out_dir.display()))
        .arg(format!("http://127.0.0.1:{port}/"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    // While the app is up (it quits by itself after 8 seconds), call its MCP server the
    // way an outside agent does: through the endpoint file and bin/revb.mjs.
    let mcp = |args: &[&str]| {
        Command::new("node")
            .arg(root.join("bin/revb.mjs"))
            .args(args)
            .env("REVER_USER_DATA_DIR", &user_data)
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_default()
    };
    let endpoint = user_data.join("control-endpoint.json");
    for _ in 0..40 {
        if endpoint.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // The fixture's fetch and XHR need a moment to be captured.
    std::thread::sleep(Duration::from_secs(3));
    let (mcp_tools, mcp_requests, mcp_storage) = (
        mcp(&["list"]),
        mcp(&["call", "list_requests"]),
        mcp(&["call", "local_storage_get"]),
    );
    let launched = child.and_then(|mut child| child.wait());
    let state: Value = fs::read_to_string(out_dir.join("state.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    let requests = state["requests"].as_array().cloned().unwrap_or_default();
    let find = |suffix: &str| {
        requests
            .iter()
            .find(|r| r["url"].as_str().is_some_and(|url| url.ends_with(suffix)))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let (items, login) = (find("/api/items"), find("/api/login"));
    let chat = state["chat"].as_array().cloned().unwrap_or_default();

    vec![
        (
            "app launches and exits cleanly from --self-check".into(),
            launched.as_ref().is_ok_and(|status| status.success()) && !state.is_null(),
            launched.as_ref().map_or_else(
                |e| e.to_string(),
                |status| format!("exit {:?}", status.code()),
            ),
        ),
        (
            "page title reaches the interface".into(),
            state["title"] == "port harness fixture",
            format!("title = {}", state["title"]),
        ),
        (
            "page is rendered into a frame (not blank)".into(),
            state["frameColors"].as_u64().unwrap_or(0) >= 3,
            format!(
                "frame {} with {} sampled colors",
                state["frame"], state["frameColors"]
            ),
        ),
        (
            "document, fetch and XHR requests are captured".into(),
            requests.len() >= 3 && items["status"] == 200 && items["resourceType"] == "XHR",
            format!(
                "{} requests; /api/items → {} {}",
                requests.len(),
                items["status"],
                items["resourceType"]
            ),
        ),
        (
            "POST request body is captured".into(),
            login["method"] == "POST" && login["requestPostData"] == "user=rever&pass=servo",
            format!("requestPostData = {}", login["requestPostData"]),
        ),
        (
            "response bodies are captured".into(),
            login["responseBody"]
                .as_str()
                .is_some_and(|body| body.contains("echoed"))
                && items["responseBody"]
                    .as_str()
                    .is_some_and(|body| body.contains("[1,2,3]")),
            format!("/api/items body = {}", items["responseBody"]),
        ),
        (
            "chat reaches the agent CLI and its reply streams back".into(),
            chat.len() == 2
                && chat[0]["fromUser"] == true
                && chat[0]["text"] == "hello"
                && chat[1]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("fake reply")),
            format!("chat = {}", Value::Array(chat.clone())),
        ),
        (
            "the agent may call the app's own MCP tools; a shell request is rejected".into(),
            chat.get(1)
                .and_then(|line| line["text"].as_str())
                .is_some_and(|text| text.ends_with("shell rejected, own tool allowed")),
            String::new(),
        ),
        (
            "MCP server answers an outside client with the captured requests".into(),
            mcp_requests.contains("/api/items") && mcp_requests.contains("/api/login"),
            format!("{} tools listed", mcp_tools.lines().count()),
        ),
        (
            "an MCP tool reaches the page (localStorage read through the engine)".into(),
            mcp_storage.contains("harness-key") && mcp_storage.contains("harness-value"),
            mcp_storage.chars().take(120).collect(),
        ),
    ]
}

fn files_under(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path, extensions));
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| extensions.contains(&e))
        {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// Every quoted or backticked string that directly follows `marker` (whitespace allowed between).
fn string_after(text: &str, marker: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(marker) {
        rest = &rest[at + marker.len()..];
        let trimmed = rest.trim_start();
        let Some(quote) = trimmed
            .chars()
            .next()
            .filter(|c| matches!(c, '\'' | '"' | '`'))
        else {
            continue;
        };
        if let Some(end) = trimmed[1..].find(quote) {
            found.push(trimmed[1..1 + end].to_owned());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 마커_뒤의_문자열을_줄바꿈과_따옴표_종류에_상관없이_찾는다() {
        let ts = "mcp.registerTool(\n    'list_requests',\n  {})\nmcp.registerTool(\"get_request\", {})\nregisterTool(name)";
        assert_eq!(
            string_after(ts, "registerTool("),
            ["list_requests", "get_request"]
        );
        let rust = "//! Port of `src/main/traffic-store.ts`: same limits";
        assert_eq!(
            string_after(rust, "Port of "),
            ["src/main/traffic-store.ts"]
        );
    }
}
