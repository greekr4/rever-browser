//! Scoreboard for the port from TypeScript/Electron to Rust/GPUI.
//!
//!   cargo run -p port-harness            # full run: inventory, coverage, behavior checks
//!   cargo run -p port-harness -- --no-run  # inventory and coverage only (no build, no app launch)
//!
//! Coverage is read from the sources, not from a hand-kept list:
//!   * a Rust file counts as the port of a TypeScript file when it says
//!     "Port of `src/...`" in a comment;
//!   * an MCP tool counts as ported when a Rust file registers it as `ToolDef::new("name"`.
//! Behavior checks launch the real app against a fixture server started here.
//! The result is also written to target/port-status.json. Exit code 1 if a check fails.

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

const FIXTURE_PAGE: &str = "<!doctype html><title>port harness fixture</title><h1>port harness fixture</h1>\
<p style=\"color:#b00;background:#eef\">colored text so the frame is not one flat color</p>\
<script>fetch('/api/items').then(function (r) { return r.json() });\
var x = new XMLHttpRequest(); x.open('POST', '/api/login'); x.send('user=rever&pass=servo')</script>";

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf();
    let run_checks = !std::env::args().any(|arg| arg == "--no-run");

    let ts_files = files_under(&root.join("src"), &["ts", "tsx"]);
    let rust_files: Vec<PathBuf> = ["rever", "rever-core"]
        .iter()
        .flat_map(|krate| files_under(&root.join(krate).join("src"), &["rs"]))
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

    println!("== Inventory (TypeScript/Electron app) ==");
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
        let (ok, detail) = cargo(&["test", "-p", "rever-core"]);
        checks.push(("core logic unit tests pass".into(), ok, detail));
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
    let launched = Command::new(root.join("target/debug/rever"))
        .arg(format!("--self-check={}", out_dir.display()))
        .arg(format!("http://127.0.0.1:{port}/"))
        .output();
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

    vec![
        (
            "app launches and exits cleanly from --self-check".into(),
            launched.as_ref().is_ok_and(|out| out.status.success()) && !state.is_null(),
            launched.as_ref().map_or_else(
                |e| e.to_string(),
                |out| format!("exit {:?}", out.status.code()),
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
