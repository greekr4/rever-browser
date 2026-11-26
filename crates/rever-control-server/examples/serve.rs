//! Runs the MCP server over a few stand-in tools, for driving it from outside:
//!
//!   REVER_USER_DATA_DIR=/tmp/rever-data cargo run -p rever-control-server --example serve
//!   REVER_USER_DATA_DIR=/tmp/rever-data node bin/revb.mjs list
//!
//! Stops when stdin closes or a line is entered.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rever_tools_api::tool::{BoxFuture, NoBrowser, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ResultStore, ToolResult, ok};
use serde_json::{Value, json};

/// Answers with its own name and arguments.
struct Echo(&'static str);

impl Tool for Echo {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            self.0,
            "Stand-in tool: echoes its arguments.",
            json!({ "type": "object" }),
        )
    }

    fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { ok(format!("{} {args}", self.0)) })
    }
}

/// Answers with more text than one result may carry, to exercise paging.
struct Big;

impl Tool for Big {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "get_request",
            "Stand-in tool: a result long enough to be truncated.",
            json!({ "type": "object" }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, _: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let lines: Vec<String> = (0..2_000).map(|n| format!("line {n}")).collect();
            let store = ctx.state::<Mutex<ResultStore>>();
            let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
            store.ok_budgeted(lines.join("\n"), None)
        })
    }
}

fn main() -> Result<(), String> {
    let data_dir = std::env::var_os("REVER_USER_DATA_DIR")
        .map(PathBuf::from)
        .ok_or("set REVER_USER_DATA_DIR to the folder control-endpoint.json goes in")?;
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;

    let mut registry = ToolRegistry::default();
    for name in [
        "list_requests",
        "browser_snapshot",
        "grep_scripts",
        "intruder_run",
    ] {
        registry.register(Echo(name));
    }
    registry.register(Big);
    let context = ToolContext::new(Arc::default(), Arc::new(NoBrowser), data_dir);

    let server = rever_control_server::start(Arc::new(registry), Arc::new(context))?;
    println!("{}", server.url);
    let _ = std::io::stdin().read_line(&mut String::new());
    server.close();
    Ok(())
}
