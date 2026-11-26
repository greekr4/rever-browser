//! The control socket end to end: a real server, a real client, no MCP.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rever_control_server::{start, ServerOptions};
use rever_tools_api::tool::{
    BoxFuture, NoBrowser, Tool, ToolContext, ToolDef, ToolRegistry,
};
use rever_tools_api::utils::{ToolResult, ok};
use serde_json::{Value, json};

/// Answers with its own name and arguments.
struct Echo;

impl Tool for Echo {
    fn def(&self) -> ToolDef {
        ToolDef::new("echo", "Echoes its arguments.", json!({ "type": "object" }))
    }

    fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { ok(format!("echo {args}")) })
    }
}

fn data_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let dir = std::env::temp_dir().join(format!("rever-socket-{label}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A running server and a client that already knows its token.
struct Fixture {
    dir: PathBuf,
    server: rever_control_server::ToolServer,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = data_dir(label);
        let mut registry = ToolRegistry::default();
        registry.register(Echo);
        let context = ToolContext::new(
            Arc::new(Mutex::new(Default::default())),
            Arc::new(NoBrowser),
            dir.clone(),
        );
        let server = start(Arc::new(registry), Arc::new(context)).unwrap();
        Self { dir, server }
    }

    fn send(&self, line: &str) -> Value {
        let stream = UnixStream::connect(self.server.url.as_str()).unwrap();
        let mut writer = stream.try_clone().unwrap();
        writeln!(writer, "{line}").unwrap();
        writer.flush().unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        serde_json::from_str(&reply).unwrap()
    }

    fn call(&self, tool: &str, args: Value) -> Value {
        let line = json!({
            "token": self.server.token,
            "tool": tool,
            "args": args,
        })
        .to_string();
        self.send(&line)
    }
}

#[test]
fn 소켓으로_도구를_부르면_결과가_온다() {
    let fixture = Fixture::new("call");
    let reply = fixture.call("echo", json!({ "n": 1 }));
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["result"]["content"][0]["type"], "text");
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("echo"));
    drop(fixture.server);
}

#[test]
fn 도구_목록은_서버가_스스로_더한_둘까지_준다() {
    let fixture = Fixture::new("catalog");
    let reply = fixture.call("__catalog", Value::Null);
    let names: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"echo"));
    assert!(names.contains(&"tool_result_more"));
    assert!(names.contains(&"tool_profile_set"));
    drop(fixture.server);
}

#[test]
fn 모르는_도구는_오류로_답한다() {
    let fixture = Fixture::new("unknown");
    let reply = fixture.call("no_such_tool", json!({}));
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap().contains("no_such_tool"));
    drop(fixture.server);
}

#[test]
fn 토큰이_없으면_거절한다() {
    let fixture = Fixture::new("token");
    let reply = fixture.send(&json!({ "tool": "echo" }).to_string());
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"], "bad token");
    drop(fixture.server);
}

#[test]
fn 엔드포인트_파일에_소켓과_토큰을_적는다() {
    let fixture = Fixture::new("endpoint");
    let file = fixture.dir.join("control-endpoint.json");
    let endpoint: Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(endpoint["socket"], fixture.server.url.as_str());
    assert_eq!(endpoint["token"], fixture.server.token.as_str());
    assert_eq!(endpoint["pid"], std::process::id());
    drop(fixture.server);
    assert!(!file.exists(), "닫으면 발행한 파일을 지운다");
}

#[test]
fn 버전은_서버_옵션에서_온다() {
    let dir = data_dir("version");
    let mut registry = ToolRegistry::default();
    registry.register(Echo);
    let context = ToolContext::new(
        Arc::new(Mutex::new(Default::default())),
        Arc::new(NoBrowser),
        dir,
    );
    let options = ServerOptions {
        version: "9.9.9".to_owned(),
        ..ServerOptions::default()
    };
    let server = rever_control_server::start_with(Arc::new(registry), Arc::new(context), options).unwrap();
    let stream = UnixStream::connect(server.url.as_str()).unwrap();
    let mut writer = stream.try_clone().unwrap();
    writeln!(
        writer,
        "{}",
        json!({ "token": server.token, "tool": "__catalog" })
    )
    .unwrap();
    writer.flush().unwrap();
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).unwrap();
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["result"]["version"], "9.9.9");
}
