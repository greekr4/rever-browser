//! Saved workflows, and the operations the macro and CLI tools run on them.
//!
//! In the TypeScript the list lives in a renderer zustand store (persisted as
//! `rev:workflows`) and the tools, which run in the main process, ask the
//! renderer for each operation: `askRenderer(op, payload)`, answered by
//! `handleAgentRequest` in src/renderer/src/workflows/core/agent-bridge.ts.
//! Here there is no renderer to ask. [`ask`] answers the same operations with
//! the same payloads and results, on a file in the app's data folder that holds
//! what zustand persisted: `{"state":{"workflows":[…]},"version":0}`.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use regex::{Captures, Regex};
use rever_cli_spec::cli_spec::{CliCommand, CliData, parse_extract};
use rever_tools_api::tool::ToolContext;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::js::{
    JS_SPACE, array_index, js_string, js_string_at, js_trim, js_truthy, json_stringify, new_id,
    now_ms,
};
use crate::lock;

/// The file under `ToolContext::data_dir`.
pub const WORKFLOWS_FILE: &str = "workflows.json";
const MAX_WORKFLOWS: usize = 200;

/// A saved workflow. `data` is whatever its kind keeps: steps and variables for
/// a macro, the compiled commands for a CLI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub id: String,
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub data: Value,
    pub created_at: u64,
    pub updated_at: u64,
    // Whatever else an entry carries survives a rewrite of the file.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    state: PersistedState,
    #[serde(default)]
    version: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedState {
    workflows: Vec<Workflow>,
}

/// Serialises the read-change-write of the file between tool calls.
#[derive(Default)]
pub struct WorkflowStore(Mutex<()>);

// A file that exists but cannot be read is an error rather than an empty list:
// the next save would otherwise replace every saved workflow.
fn load(path: &Path) -> Result<Vec<Workflow>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {WORKFLOWS_FILE}: {e}")),
    };
    let persisted: Persisted = serde_json::from_str(&text)
        .map_err(|e| format!("{WORKFLOWS_FILE} is not a saved workflow list: {e}"))?;
    Ok(persisted.state.workflows)
}

// Through a temporary file and a rename, so a crash mid-write leaves the
// previous file intact.
fn save(path: &Path, workflows: Vec<Workflow>) -> Result<(), String> {
    let persisted = Persisted {
        state: PersistedState { workflows },
        version: 0,
    };
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut file = fs::File::create(&tmp)?;
        file.write_all(serde_json::to_string(&persisted)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| format!("cannot write {WORKFLOWS_FILE}: {e}"))
}

/// `askRenderer(op, payload)` for the `macro:*` and `cli:*` operations.
pub fn ask(ctx: &ToolContext, op: &str, payload: Value) -> Result<Value, String> {
    let store = ctx.state::<WorkflowStore>();
    let _serial = lock(&store.0);
    let path = ctx.data_dir.join(WORKFLOWS_FILE);
    let mut list = List {
        workflows: load(&path)?,
        changed: false,
    };
    let answer = handle_agent_request(&mut list, op, &payload)?;
    if list.changed {
        save(&path, list.workflows)?;
    }
    Ok(answer)
}

struct List {
    workflows: Vec<Workflow>,
    changed: bool,
}

impl List {
    /// Insert or replace by id, keeping the list newest-first.
    fn upsert(&mut self, mut workflow: Workflow) {
        self.workflows.retain(|other| other.id != workflow.id);
        workflow.updated_at = now_ms();
        self.workflows.insert(0, workflow);
        self.workflows.truncate(MAX_WORKFLOWS);
        self.changed = true;
    }

    fn remove(&mut self, id: &str) {
        self.workflows.retain(|other| other.id != id);
        self.changed = true;
    }

    fn find(&self, kind: &str, id: &str) -> Option<&Workflow> {
        self.workflows
            .iter()
            .find(|workflow| workflow.kind == kind && workflow.id == id)
    }

    fn find_macro(&self, id: &str) -> Result<Workflow, String> {
        self.find("macro", id)
            .cloned()
            .ok_or_else(|| format!("No macro with id \"{id}\""))
    }

    fn find_cli(&self, id: &str) -> Result<Workflow, String> {
        self.find("cli", id)
            .cloned()
            .ok_or_else(|| format!("No CLI with id \"{id}\""))
    }
}

fn steps_of(data: &Value) -> &[Value] {
    data.get("steps")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn text_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The step shape the agent sends becomes the stored one: the input as JSON text.
fn to_macro_steps(steps: &Value) -> Vec<Value> {
    let steps = steps.as_array().map_or(&[][..], Vec::as_slice);
    steps
        .iter()
        .map(|step| {
            let input = step.get("input").filter(|input| !input.is_null());
            let mut stored = Map::new();
            stored.insert("id".to_owned(), json!(new_id('s', 4)));
            stored.insert(
                "tool".to_owned(),
                step.get("tool").cloned().unwrap_or_default(),
            );
            stored.insert(
                "input".to_owned(),
                json!(json_stringify(input.unwrap_or(&json!({})), true)),
            );
            if let Some(wait_for) = step.get("waitFor").filter(|value| js_truthy(value)) {
                stored.insert("waitFor".to_owned(), wait_for.clone());
            }
            if let Some(delay) = step.get("delay").filter(|value| !value.is_null()) {
                stored.insert("delay".to_owned(), delay.clone());
            }
            Value::Object(stored)
        })
        .collect()
}

/// The stored step shape becomes the one the agent reads: the input parsed.
fn to_agent_steps(steps: &[Value]) -> Vec<Value> {
    steps
        .iter()
        .map(|step| {
            let tool = step.get("tool").cloned().unwrap_or_default();
            let text = text_of(step, "input");
            let input = if js_trim(text).is_empty() {
                Ok(json!({}))
            } else {
                serde_json::from_str::<Value>(text)
            };
            let Ok(input) = input else {
                // Keep the raw text visible to the agent so it can fix a broken step.
                return json!({ "tool": tool, "input": { "__unparsed": text } });
            };
            let mut shown = Map::new();
            shown.insert("tool".to_owned(), tool);
            shown.insert("input".to_owned(), input);
            if let Some(wait_for) = step.get("waitFor").filter(|value| js_truthy(value)) {
                shown.insert("waitFor".to_owned(), wait_for.clone());
            }
            if let Some(delay) = step.get("delay").filter(|value| !value.is_null()) {
                shown.insert("delay".to_owned(), delay.clone());
            }
            Value::Object(shown)
        })
        .collect()
}

fn summary(workflow: &Workflow) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("id".to_owned(), json!(workflow.id));
    out.insert("name".to_owned(), json!(workflow.name));
    out.insert(
        "description".to_owned(),
        json!(workflow.description.as_deref().unwrap_or("")),
    );
    out.insert(
        "stepCount".to_owned(),
        json!(steps_of(&workflow.data).len()),
    );
    out.insert("updatedAt".to_owned(), json!(workflow.updated_at));
    out
}

fn detail(workflow: &Workflow) -> Value {
    let mut out = summary(workflow);
    out.insert(
        "steps".to_owned(),
        Value::Array(to_agent_steps(steps_of(&workflow.data))),
    );
    if let Some(vars) = workflow.data.get("vars") {
        out.insert("vars".to_owned(), vars.clone());
    }
    Value::Object(out)
}

static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    // `\w` is ASCII-only in JavaScript.
    Regex::new(&format!(
        r"\{{\{{{JS_SPACE}*([A-Za-z0-9_.-]+){JS_SPACE}*\}}\}}"
    ))
    .expect("built-in pattern compiles")
});

/// `vars[key]` for the value `JSON.parse` gave: only own properties, which is
/// all a parsed object has besides what it inherits.
// shortcut: inherited names ({{constructor}}, {{toString}}) and the characters of
// a string given as the whole variables text are not looked up; they stay as
// written. Upgrade if a macro is ever found relying on them.
fn var_of(vars: &Value, key: &str) -> Option<Value> {
    let found = match vars {
        Value::Object(map) => map.get(key).cloned(),
        Value::Array(items) if key == "length" => Some(json!(items.len())),
        Value::Array(items) => array_index(key).and_then(|index| items.get(index).cloned()),
        _ => None,
    };
    found.filter(|value| !value.is_null())
}

/// `resolveSteps` of the macro editor: substitute `{{var}}`, then parse each
/// step's input into a runnable form.
fn resolve_steps(steps: &[Value], vars_text: &str) -> Result<Vec<Value>, String> {
    let vars = if js_trim(vars_text).is_empty() {
        json!({})
    } else {
        serde_json::from_str(vars_text).map_err(|_| "Variables are not valid JSON")?
    };
    let mut out = Vec::new();
    for step in steps {
        let tool = text_of(step, "tool");
        if tool.is_empty() {
            return Err("A step has no tool selected".to_owned());
        }
        // Placeholders with no matching variable are left in place: they may refer
        // to a value an earlier step captures at run time via saveAs, which only
        // the executor can resolve.
        let substituted = PLACEHOLDER.replace_all(text_of(step, "input"), |found: &Captures| {
            let whole = found.get(0).map_or("", |m| m.as_str());
            match var_of(&vars, found.get(1).map_or("", |m| m.as_str())) {
                None => whole.to_owned(),
                // Escape so an injected value can't break out of the JSON string.
                Some(value) => {
                    let quoted = Value::from(js_string(&value)).to_string();
                    quoted
                        .get(1..quoted.len().saturating_sub(1))
                        .unwrap_or("")
                        .to_owned()
                }
            }
        });
        let input = if js_trim(&substituted).is_empty() {
            json!({})
        } else {
            serde_json::from_str::<Value>(&substituted)
                .map_err(|_| format!("Step \"{tool}\" input is not valid JSON"))?
        };
        let mut resolved = Map::new();
        resolved.insert("tool".to_owned(), json!(tool));
        resolved.insert("input".to_owned(), input);
        let trimmed = |key: &str| Some(js_trim(text_of(step, key))).filter(|text| !text.is_empty());
        if let Some(wait_for) = trimmed("waitFor") {
            resolved.insert("waitFor".to_owned(), json!(wait_for));
        }
        let delay = step
            .get("delay")
            .filter(|delay| delay.as_f64().is_some_and(|ms| ms > 0.0));
        if let Some(delay) = delay {
            resolved.insert("delay".to_owned(), delay.clone());
        }
        if let Some(save_as) = trimmed("saveAs") {
            resolved.insert("saveAs".to_owned(), json!(save_as));
        }
        out.push(Value::Object(resolved));
    }
    Ok(out)
}

/// The saved payload as a CLI. A command that does not have the saved shape is
/// an error here: the TypeScript carried it along untyped, which this port
/// cannot, and dropping it would lose it on the next save. A broken `extract`
/// alone is dropped while loading, as `attachExtract` did.
fn as_cli(data: &Value) -> Result<CliData, String> {
    let commands = data.get("commands").and_then(Value::as_array);
    let commands = commands
        .into_iter()
        .flatten()
        .map(|command| serde_json::from_value::<CliCommand>(command.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("a saved CLI command is not readable: {e}"))?;
    Ok(CliData {
        site: text_of(data, "site").to_owned(),
        title: text_of(data, "title").to_owned(),
        base_url: text_of(data, "baseUrl").to_owned(),
        commands,
        writes: data.get("writes") == Some(&Value::Bool(true)),
    })
}

fn cli_value(data: &CliData) -> Result<Value, String> {
    serde_json::to_value(data).map_err(|e| e.to_string())
}

fn cli_summary(workflow: &Workflow) -> Result<Value, String> {
    let data = as_cli(&workflow.data)?;
    let commands: Vec<Value> = data
        .commands
        .iter()
        .map(|command| {
            let mut out = Map::new();
            out.insert("name".to_owned(), json!(command.name));
            out.insert("method".to_owned(), json!(command.method));
            out.insert("path".to_owned(), json!(command.path));
            out.insert("effect".to_owned(), json!(command.effect));
            if let Some(extract) = &command.extract {
                let names: Vec<&str> = extract.fields.iter().map(|f| f.name.as_str()).collect();
                out.insert("extract".to_owned(), json!(names.join(", ")));
            }
            Value::Object(out)
        })
        .collect();
    Ok(json!({
        "id": workflow.id,
        "name": workflow.name,
        "site": data.site,
        "baseUrl": data.base_url,
        "writes": data.writes,
        "commands": commands,
    }))
}

fn handle_agent_request(list: &mut List, op: &str, p: &Value) -> Result<Value, String> {
    let id = js_string_at(p, "id");
    // `x != null`: present and not null.
    let given = |key: &str| p.get(key).filter(|value| !value.is_null());
    match op {
        "macro:list" => {
            let macros = list.workflows.iter().filter(|w| w.kind == "macro");
            Ok(macros.map(|w| Value::Object(summary(w))).collect())
        }

        "macro:get" => Ok(detail(&list.find_macro(&id)?)),

        "macro:create" => {
            let now = now_ms();
            let truthy = |key: &str| p.get(key).filter(|value| js_truthy(value));
            let workflow = Workflow {
                id: new_id('w', 6),
                kind: "macro".to_owned(),
                name: given("name").map_or_else(|| "Untitled macro".to_owned(), js_string),
                description: truthy("description").map(js_string),
                data: json!({
                    "steps": to_macro_steps(p.get("steps").unwrap_or(&Value::Null)),
                    "vars": truthy("vars").map(js_string).unwrap_or_default(),
                }),
                created_at: now,
                updated_at: now,
                extra: Map::new(),
            };
            list.upsert(workflow.clone());
            Ok(detail(&workflow))
        }

        "macro:update" => {
            let mut next = list.find_macro(&id)?;
            if let Some(name) = given("name") {
                next.name = js_string(name);
            }
            if let Some(description) = given("description") {
                next.description = Some(js_string(description));
            }
            let mut data = Map::new();
            let steps = match given("steps") {
                Some(steps) => to_macro_steps(steps),
                None => steps_of(&next.data).to_vec(),
            };
            data.insert("steps".to_owned(), Value::Array(steps));
            let vars = given("vars").map(|vars| json!(js_string(vars)));
            if let Some(vars) = vars.or_else(|| next.data.get("vars").cloned()) {
                data.insert("vars".to_owned(), vars);
            }
            next.data = Value::Object(data);
            list.upsert(next.clone());
            Ok(detail(&next))
        }

        "macro:remove-steps" => {
            let mut next = list.find_macro(&id)?;
            // As numbers, so 1.0 names the same position as 1.
            let indexes: Vec<f64> = given("indexes")
                .and_then(Value::as_array)
                .map(|indexes| indexes.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default();
            let kept: Vec<Value> = steps_of(&next.data)
                .iter()
                .enumerate()
                .filter(|(index, _)| !indexes.contains(&(*index as f64)))
                .map(|(_, step)| step.clone())
                .collect();
            match &mut next.data {
                Value::Object(data) => {
                    data.insert("steps".to_owned(), Value::Array(kept));
                }
                other => *other = json!({ "steps": kept }),
            }
            list.upsert(next.clone());
            Ok(detail(&next))
        }

        // run_macro asks here rather than resolving on its own, so {{var}}
        // substitution has exactly one implementation.
        "macro:resolve" => {
            let workflow = list.find_macro(&id)?;
            let vars = match given("vars") {
                Some(vars) => js_string(vars),
                None => text_of(&workflow.data, "vars").to_owned(),
            };
            let steps = resolve_steps(steps_of(&workflow.data), &vars)?;
            if steps.is_empty() {
                return Err(format!("Macro \"{}\" has no steps", workflow.name));
            }
            Ok(json!({ "name": workflow.name, "steps": steps }))
        }

        "macro:delete" => {
            list.find_macro(&id)?;
            list.remove(&id);
            Ok(json!({ "deleted": true }))
        }

        "cli:list" => {
            let clis = list.workflows.iter().filter(|w| w.kind == "cli");
            clis.map(cli_summary).collect()
        }

        "cli:get" => {
            let workflow = list.find_cli(&id)?;
            let data = cli_value(&as_cli(&workflow.data)?)?;
            Ok(json!({ "id": workflow.id, "name": workflow.name, "data": data }))
        }

        "cli:save" => {
            let mut incoming = as_cli(p.get("data").unwrap_or(&Value::Null))?;
            if incoming.commands.is_empty() {
                return Err("CLI has no commands".to_owned());
            }
            let now = now_ms();
            let id = match p.get("id").and_then(Value::as_str) {
                Some(id) if !id.is_empty() => id.to_owned(),
                _ => new_id('w', 6),
            };
            let existing = list.find("cli", &id);
            let name = match (given("name"), existing) {
                (Some(name), _) => js_string(name),
                (None, Some(existing)) => existing.name.clone(),
                (None, None) => incoming.site.clone(),
            };
            let created_at = existing.map_or(now, |existing| existing.created_at);
            // A CLI is always saved with writes off; cli_writes turns them on.
            incoming.writes = false;
            let workflow = Workflow {
                id,
                kind: "cli".to_owned(),
                name,
                description: None,
                data: cli_value(&incoming)?,
                created_at,
                updated_at: now,
                extra: Map::new(),
            };
            list.upsert(workflow.clone());
            cli_summary(&workflow)
        }

        "cli:set-writes" => {
            let mut next = list.find_cli(&id)?;
            let mut data = as_cli(&next.data)?;
            data.writes = p.get("writes") == Some(&Value::Bool(true));
            next.data = cli_value(&data)?;
            list.upsert(next.clone());
            cli_summary(&next)
        }

        "cli:set-extract" => {
            let mut next = list.find_cli(&id)?;
            let mut data = as_cli(&next.data)?;
            let name = given("command").map(js_string).unwrap_or_default();
            let extract = parse_extract(p.get("extract").unwrap_or(&Value::Null))?
                .ok_or("extract is required")?;
            let command = data.commands.iter_mut().find(|item| item.name == name);
            let command = command.ok_or_else(|| format!("unknown command: {name}"))?;
            command.extract = Some(extract);
            next.data = cli_value(&data)?;
            list.upsert(next.clone());
            cli_summary(&next)
        }

        "cli:effect" => {
            let data = as_cli(&list.find_cli(&id)?.data)?;
            let name = js_string_at(p, "command");
            let command = data.commands.iter().find(|item| item.name == name);
            Ok(command.map_or(json!("unknown"), |command| json!(command.effect)))
        }

        _ => Err(format!("Unknown bridge op \"{op}\"")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{capture_shop, context};

    fn saved(ctx: &ToolContext) -> Value {
        let text = fs::read_to_string(ctx.data_dir.join(WORKFLOWS_FILE)).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// Writes the file the way the Workflows panel would have left it.
    fn seed(ctx: &ToolContext, workflows: Value) {
        fs::create_dir_all(&ctx.data_dir).unwrap();
        let file = json!({ "state": { "workflows": workflows }, "version": 0 });
        fs::write(ctx.data_dir.join(WORKFLOWS_FILE), file.to_string()).unwrap();
    }

    fn panel_macro(id: &str, steps: Value, vars: &str) -> Value {
        json!({
            "id": id, "kind": "macro", "name": format!("Macro {id}"),
            "data": { "steps": steps, "vars": vars },
            "createdAt": 1000, "updatedAt": 2000
        })
    }

    fn create(ctx: &ToolContext, payload: Value) -> Value {
        ask(ctx, "macro:create", payload).unwrap()
    }

    #[test]
    fn 파일이_없으면_빈_목록이고_저장하면_zustand_봉투_모양으로_쓴다() {
        let ctx = context();
        assert_eq!(ask(&ctx, "macro:list", json!({})), Ok(json!([])));
        assert_eq!(ask(&ctx, "cli:list", json!({})), Ok(json!([])));
        // Reading does not create the file.
        assert!(!ctx.data_dir.join(WORKFLOWS_FILE).exists());

        let made = create(&ctx, json!({ "name": "m", "steps": [{ "tool": "t" }] }));
        let file = saved(&ctx);
        assert_eq!(file["version"], 0);
        let entry = &file["state"]["workflows"][0];
        assert_eq!(entry["id"], made["id"]);
        assert_eq!(entry["kind"], "macro");
        assert_eq!(entry["data"]["vars"], "");
        assert_eq!(entry["data"]["steps"][0]["input"], "{}");
        assert!(entry.get("description").is_none());
        assert!(entry["createdAt"].is_u64() && entry["updatedAt"].is_u64());
        assert!(!ctx.data_dir.join("workflows.json.tmp").exists());
    }

    #[test]
    fn 읽을_수_없는_파일은_덮어쓰지_않고_에러를_낸다() {
        let ctx = context();
        fs::create_dir_all(&ctx.data_dir).unwrap();
        let path = ctx.data_dir.join(WORKFLOWS_FILE);
        for broken in ["{ not json", "[]", r#"{"state":{"workflows":[{"id":5}]}}"#] {
            fs::write(&path, broken).unwrap();
            let error = ask(&ctx, "macro:create", json!({ "name": "m", "steps": [] })).unwrap_err();
            assert!(
                error.starts_with("workflows.json is not a saved workflow list: "),
                "{error}"
            );
            assert!(ask(&ctx, "macro:list", json!({})).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), broken);
        }
    }

    #[test]
    fn 매크로를_만들면_입력을_json_글로_저장하고_상세를_돌려준다() {
        let ctx = context();
        let made = create(
            &ctx,
            json!({
                "name": "Login",
                "description": "Signs in",
                "steps": [
                    { "tool": "browser_click", "input": { "ref": "r1", "n": 2.0 }, "waitFor": "#go", "delay": 250 },
                    // waitTimeout and saveAs are in the tool's schema, but the
                    // TypeScript bridge keeps neither when saving.
                    { "tool": "browser_type", "waitFor": "", "waitTimeout": 5, "saveAs": "x" }
                ],
                "vars": "{\"user\":\"kim\"}"
            }),
        );
        // Key order as the TypeScript object has it.
        let keys: Vec<&String> = made.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "id",
                "name",
                "description",
                "stepCount",
                "updatedAt",
                "steps",
                "vars"
            ]
        );
        assert_eq!(made["name"], "Login");
        assert_eq!(made["description"], "Signs in");
        assert_eq!(made["stepCount"], 2);
        assert_eq!(made["vars"], "{\"user\":\"kim\"}");
        assert_eq!(
            made["steps"],
            json!([
                { "tool": "browser_click", "input": { "ref": "r1", "n": 2 }, "waitFor": "#go", "delay": 250 },
                { "tool": "browser_type", "input": {} }
            ])
        );
        assert!(made["id"].as_str().unwrap().starts_with('w'));

        let stored = &saved(&ctx)["state"]["workflows"][0]["data"]["steps"];
        // JSON.stringify(input, null, 2), where 2.0 prints as 2.
        assert_eq!(stored[0]["input"], "{\n  \"ref\": \"r1\",\n  \"n\": 2\n}");
        assert!(stored[0]["id"].as_str().unwrap().starts_with('s'));
        assert_ne!(stored[0]["id"], stored[1]["id"]);
        let keys: Vec<&String> = stored[1].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["id", "tool", "input"]);

        // Defaults: no name, no description, no vars.
        let bare = create(&ctx, json!({ "steps": [] }));
        assert_eq!(bare["name"], "Untitled macro");
        assert_eq!(bare["description"], "");
        assert_eq!(bare["vars"], "");
    }

    #[test]
    fn 목록은_최신순이고_매크로만_요약한다() {
        let ctx = context();
        seed(
            &ctx,
            json!([
                { "id": "p1", "kind": "pipeline", "name": "Pipe", "data": { "nodes": [] }, "createdAt": 1, "updatedAt": 2, "pinned": true },
                panel_macro("m1", json!([{ "id": "s1", "tool": "t", "input": "" }]), "")
            ]),
        );
        let made = create(&ctx, json!({ "name": "new", "steps": [] }));
        let list = ask(&ctx, "macro:list", json!({})).unwrap();
        assert_eq!(
            list,
            json!([
                { "id": made["id"], "name": "new", "description": "", "stepCount": 0, "updatedAt": made["updatedAt"] },
                { "id": "m1", "name": "Macro m1", "description": "", "stepCount": 1, "updatedAt": 2000 }
            ])
        );
        // Other kinds, and fields this crate does not know, survive the rewrite.
        let file = saved(&ctx);
        let kept = &file["state"]["workflows"][1];
        assert_eq!(kept["id"], "p1");
        assert_eq!(kept["pinned"], true);
        assert_eq!(kept["data"], json!({ "nodes": [] }));
        // A macro id does not find a workflow of another kind.
        assert_eq!(
            ask(&ctx, "macro:get", json!({ "id": "p1" })),
            Err("No macro with id \"p1\"".to_owned())
        );
    }

    #[test]
    fn 저장된_워크플로는_200개까지만_남긴다() {
        let ctx = context();
        let many: Vec<Value> = (0..MAX_WORKFLOWS)
            .map(|n| panel_macro(&format!("m{n}"), json!([]), ""))
            .collect();
        seed(&ctx, json!(many));
        create(&ctx, json!({ "name": "one more", "steps": [] }));
        let file = saved(&ctx);
        let list = file["state"]["workflows"].as_array().unwrap();
        assert_eq!(list.len(), 200);
        assert_eq!(list[0]["name"], "one more");
        // The oldest entry, at the end, is the one that goes.
        assert_eq!(list[199]["id"], "m198");
    }

    #[test]
    fn 상세는_깨진_입력을_그대로_보여_준다() {
        let ctx = context();
        seed(
            &ctx,
            json!([panel_macro(
                "m1",
                json!([
                    { "id": "s1", "tool": "a", "input": "{ broken", "waitFor": "#x", "delay": 5 },
                    { "id": "s2", "tool": "b", "input": "  ", "delay": 0 },
                    { "id": "s3", "tool": "c", "input": "[1, 2]", "saveAs": "r" }
                ]),
                "{}"
            )]),
        );
        let detail = ask(&ctx, "macro:get", json!({ "id": "m1" })).unwrap();
        assert_eq!(
            detail["steps"],
            json!([
                // Keep the raw text visible; the waits are not shown for such a step.
                { "tool": "a", "input": { "__unparsed": "{ broken" } },
                { "tool": "b", "input": {}, "delay": 0 },
                { "tool": "c", "input": [1, 2] }
            ])
        );
        assert_eq!(detail["updatedAt"], 2000);
    }

    #[test]
    fn 수정은_준_필드만_바꾸고_스텝은_통째로_바꾼다() {
        let ctx = context();
        let made = create(
            &ctx,
            json!({ "name": "old", "description": "d", "steps": [{ "tool": "a" }, { "tool": "b" }], "vars": "{}" }),
        );
        let id = made["id"].clone();
        let renamed = ask(&ctx, "macro:update", json!({ "id": id, "name": "new" })).unwrap();
        assert_eq!(renamed["name"], "new");
        assert_eq!(renamed["description"], "d");
        assert_eq!(renamed["stepCount"], 2);
        assert_eq!(renamed["vars"], "{}");

        let replaced = ask(
            &ctx,
            "macro:update",
            json!({ "id": id, "steps": [{ "tool": "c", "input": { "k": "{{v}}" } }], "vars": "{\"v\":1}", "description": "" }),
        )
        .unwrap();
        assert_eq!(
            replaced["steps"],
            json!([{ "tool": "c", "input": { "k": "{{v}}" } }])
        );
        assert_eq!(replaced["vars"], "{\"v\":1}");
        // An empty description is still a description given.
        assert_eq!(replaced["description"], "");
        assert_eq!(
            ask(&ctx, "macro:list", json!({}))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            ask(&ctx, "macro:update", json!({ "id": "nope", "name": "x" })),
            Err("No macro with id \"nope\"".to_owned())
        );
    }

    #[test]
    fn 위치로_스텝을_지우고_매크로를_삭제한다() {
        let ctx = context();
        let made = create(
            &ctx,
            json!({ "name": "m", "steps": [{ "tool": "a" }, { "tool": "b" }, { "tool": "c" }, { "tool": "d" }] }),
        );
        let id = made["id"].clone();
        let left = ask(
            &ctx,
            "macro:remove-steps",
            json!({ "id": id, "indexes": [0, 2.0, 9] }),
        )
        .unwrap();
        let tools: Vec<&str> = left["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["tool"].as_str().unwrap())
            .collect();
        assert_eq!(tools, ["b", "d"]);
        assert_eq!(left["stepCount"], 2);
        // The variables text is untouched by a step removal.
        assert_eq!(saved(&ctx)["state"]["workflows"][0]["data"]["vars"], "");

        assert_eq!(
            ask(&ctx, "macro:delete", json!({ "id": id })),
            Ok(json!({ "deleted": true }))
        );
        assert_eq!(ask(&ctx, "macro:list", json!({})), Ok(json!([])));
        assert_eq!(
            ask(&ctx, "macro:delete", json!({ "id": id })),
            Err(format!("No macro with id \"{}\"", id.as_str().unwrap()))
        );
        assert_eq!(
            ask(&ctx, "macro:delete", json!({})),
            Err("No macro with id \"undefined\"".to_owned())
        );
    }

    #[test]
    fn 실행용_스텝은_변수를_채우고_입력을_파싱한다() {
        let ctx = context();
        seed(
            &ctx,
            json!([panel_macro(
                "m1",
                json!([
                    { "id": "s1", "tool": "type", "input": "{ \"text\": \"{{ user }} says {{quote}}\", \"n\": {{count}}, \"later\": \"{{saved}}\", \"none\": \"{{nil}}\" }",
                      "waitFor": "  #box  ", "delay": 300, "saveAs": " out " },
                    { "id": "s2", "tool": "click", "input": "", "waitFor": "   ", "delay": 0, "saveAs": "" }
                ]),
                "{ \"user\": \"kim\", \"quote\": \"\\\"hi\\\"\\n\", \"count\": 3, \"nil\": null }"
            )]),
        );
        let resolved = ask(&ctx, "macro:resolve", json!({ "id": "m1" })).unwrap();
        assert_eq!(
            resolved,
            json!({
                "name": "Macro m1",
                "steps": [
                    {
                        "tool": "type",
                        // A value cannot break out of its JSON string; a name with
                        // no value stays for the executor to fill.
                        "input": { "text": "kim says \"hi\"\n", "n": 3, "later": "{{saved}}", "none": "{{nil}}" },
                        "waitFor": "#box",
                        "delay": 300,
                        "saveAs": "out"
                    },
                    { "tool": "click", "input": {} }
                ]
            })
        );

        // Variables passed for this run replace the saved ones altogether.
        let overridden = ask(
            &ctx,
            "macro:resolve",
            json!({ "id": "m1", "vars": "{\"user\":[1,null,\"a\"],\"quote\":{},\"count\":7.0}" }),
        )
        .unwrap();
        assert_eq!(
            overridden["steps"][0]["input"],
            json!({ "text": "1,,a says [object Object]", "n": 7, "later": "{{saved}}", "none": "{{nil}}" })
        );
    }

    #[test]
    fn 변수가_배열이면_인덱스와_length로_읽는다() {
        // Expected values are what resolveSteps returns under bun.
        let ctx = context();
        let input = r#"{"a":"{{0}}","b":"{{00}}","c":"{{length}}","d":"{{1}}","e":"{{2}}"}"#;
        seed(
            &ctx,
            json!([panel_macro(
                "m1",
                json!([{ "id": "s1", "tool": "t", "input": input }]),
                "[1,2]"
            )]),
        );
        let resolve = |vars: &str| {
            let resolved = ask(&ctx, "macro:resolve", json!({ "id": "m1", "vars": vars }));
            resolved.unwrap()["steps"][0]["input"].clone()
        };
        assert_eq!(
            ask(&ctx, "macro:resolve", json!({ "id": "m1" })).unwrap()["steps"][0]["input"],
            json!({ "a": "1", "b": "{{00}}", "c": "2", "d": "2", "e": "{{2}}" })
        );
        // An object's keys are looked up as written.
        assert_eq!(
            resolve(r#"{"0":"z","00":"y","length":3}"#),
            json!({ "a": "z", "b": "y", "c": "3", "d": "{{1}}", "e": "{{2}}" })
        );
        // A number or a boolean has no variables.
        let untouched =
            json!({ "a": "{{0}}", "b": "{{00}}", "c": "{{length}}", "d": "{{1}}", "e": "{{2}}" });
        assert_eq!(resolve("5"), untouched);
        assert_eq!(resolve("true"), untouched);
    }

    #[test]
    fn 입력_전체가_플레이스홀더면_객체가_아닌_입력도_그대로_넘긴다() {
        // resolveSteps under bun: {"input":5} and {"input":null}.
        let ctx = context();
        seed(
            &ctx,
            json!([panel_macro(
                "m1",
                json!([
                    { "id": "s1", "tool": "t", "input": "{{x}}" },
                    { "id": "s2", "tool": "t", "input": "null" }
                ]),
                "{\"x\":5}"
            )]),
        );
        let resolved = ask(&ctx, "macro:resolve", json!({ "id": "m1" })).unwrap();
        assert_eq!(resolved["steps"][0]["input"], json!(5));
        assert_eq!(resolved["steps"][1]["input"], Value::Null);
    }

    #[test]
    fn 실행할_수_없는_매크로는_이유를_알린다() {
        let ctx = context();
        seed(
            &ctx,
            json!([
                panel_macro("empty", json!([]), ""),
                panel_macro(
                    "notool",
                    json!([{ "id": "s", "tool": "", "input": "{}" }]),
                    ""
                ),
                panel_macro(
                    "badjson",
                    json!([{ "id": "s", "tool": "click", "input": "{ nope" }]),
                    ""
                ),
                panel_macro(
                    "badvars",
                    json!([{ "id": "s", "tool": "click", "input": "{}" }]),
                    "{ nope"
                )
            ]),
        );
        let resolve = |id: &str| ask(&ctx, "macro:resolve", json!({ "id": id })).unwrap_err();
        assert_eq!(resolve("empty"), "Macro \"Macro empty\" has no steps");
        assert_eq!(resolve("notool"), "A step has no tool selected");
        assert_eq!(resolve("badjson"), "Step \"click\" input is not valid JSON");
        assert_eq!(resolve("badvars"), "Variables are not valid JSON");
        assert_eq!(resolve("gone"), "No macro with id \"gone\"");
        // Blank variables for this run are no variables, not an error.
        assert!(
            ask(
                &ctx,
                "macro:resolve",
                json!({ "id": "badvars", "vars": " " })
            )
            .is_ok()
        );
        assert_eq!(
            ask(&ctx, "nope:op", json!({})),
            Err("Unknown bridge op \"nope:op\"".to_owned())
        );
    }

    fn shop_cli(ctx: &ToolContext) -> Value {
        capture_shop(ctx);
        let ids: Vec<String> = ["r1", "r2"].map(str::to_owned).to_vec();
        let names = [("r1", "search"), ("r2", "create")];
        let opts = rever_cli_spec::cli_spec::CompileOptions {
            names: Some(
                names
                    .iter()
                    .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
                    .collect(),
            ),
            ..Default::default()
        };
        let mut data = crate::cli_exec::compile_captured(ctx, &ids, &opts).unwrap();
        // Whatever the caller says, a CLI is saved with writes off.
        data.writes = true;
        ask(ctx, "cli:save", json!({ "name": "Shop", "data": data })).unwrap()
    }

    #[test]
    fn cli를_저장하면_쓰기가_꺼진_요약을_돌려준다() {
        let ctx = context();
        let summary = shop_cli(&ctx);
        let id = summary["id"].clone();
        assert_eq!(
            summary,
            json!({
                "id": id,
                "name": "Shop",
                "site": "shop-test",
                "baseUrl": "https://shop.test",
                "writes": false,
                "commands": [
                    { "name": "search", "method": "GET", "path": "/api/items", "effect": "read" },
                    { "name": "create", "method": "POST", "path": "/api/items", "effect": "write" }
                ]
            })
        );
        assert_eq!(ask(&ctx, "cli:list", json!({})), Ok(json!([summary])));
        let entry = &saved(&ctx)["state"]["workflows"][0];
        assert_eq!(entry["kind"], "cli");
        assert_eq!(entry["data"]["writes"], false);
        let keys: Vec<&String> = entry["data"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["site", "title", "baseUrl", "commands", "writes"]);
        // Macro tools do not see it.
        assert_eq!(ask(&ctx, "macro:list", json!({})), Ok(json!([])));

        let got = ask(&ctx, "cli:get", json!({ "id": id })).unwrap();
        assert_eq!(got["name"], "Shop");
        assert_eq!(got["data"], entry["data"]);
        assert_eq!(
            ask(&ctx, "cli:get", json!({ "id": "nope" })),
            Err("No CLI with id \"nope\"".to_owned())
        );
        let empty = json!({ "site": "s", "title": "t", "baseUrl": "https://s.test", "commands": [], "writes": false });
        assert_eq!(
            ask(&ctx, "cli:save", json!({ "name": "x", "data": empty })),
            Err("CLI has no commands".to_owned())
        );
    }

    #[test]
    fn 같은_id로_다시_저장하면_만든_시각과_이름을_잇는다() {
        let ctx = context();
        let first = shop_cli(&ctx);
        let id = first["id"].clone();
        let before = saved(&ctx)["state"]["workflows"][0].clone();
        ask(&ctx, "cli:set-writes", json!({ "id": id, "writes": true })).unwrap();
        let again = ask(
            &ctx,
            "cli:save",
            json!({ "id": id, "data": before["data"] }),
        )
        .unwrap();
        assert_eq!(again["name"], "Shop");
        assert_eq!(again["writes"], false);
        let after = saved(&ctx);
        assert_eq!(after["state"]["workflows"].as_array().unwrap().len(), 1);
        assert_eq!(
            after["state"]["workflows"][0]["createdAt"],
            before["createdAt"]
        );
    }

    #[test]
    fn 쓰기_스위치와_추출을_저장된_cli에_반영한다() {
        let ctx = context();
        let id = shop_cli(&ctx)["id"].clone();
        let on = ask(&ctx, "cli:set-writes", json!({ "id": id, "writes": true })).unwrap();
        assert_eq!(on["writes"], true);
        // Only `true` turns writes on.
        let off = ask(&ctx, "cli:set-writes", json!({ "id": id, "writes": "yes" })).unwrap();
        assert_eq!(off["writes"], false);

        let extract = json!({ "selector": " li ", "fields": [{ "name": "title" }, { "name": "link", "attr": "href" }] });
        let set = ask(
            &ctx,
            "cli:set-extract",
            json!({ "id": id, "command": "search", "extract": extract }),
        )
        .unwrap();
        assert_eq!(set["commands"][0]["extract"], "title, link");
        assert!(set["commands"][1].get("extract").is_none());
        assert_eq!(
            saved(&ctx)["state"]["workflows"][0]["data"]["commands"][0]["extract"],
            json!({ "selector": "li", "limit": 5, "fields": [{ "name": "title" }, { "name": "link", "attr": "href" }] })
        );

        let set_extract = |command: &str, extract: Value| {
            ask(
                &ctx,
                "cli:set-extract",
                json!({ "id": id, "command": command, "extract": extract }),
            )
            .unwrap_err()
        };
        assert_eq!(set_extract("search", Value::Null), "extract is required");
        assert_eq!(
            set_extract("search", json!({ "selector": "", "fields": [] })),
            "extract.selector is required"
        );
        assert_eq!(
            set_extract("nope", extract.clone()),
            "unknown command: nope"
        );
    }

    #[test]
    fn 명령의_효과를_알려_주고_모르는_명령은_unknown이다() {
        let ctx = context();
        let id = shop_cli(&ctx)["id"].clone();
        let effect =
            |command: &str| ask(&ctx, "cli:effect", json!({ "id": id, "command": command }));
        assert_eq!(effect("search"), Ok(json!("read")));
        assert_eq!(effect("create"), Ok(json!("write")));
        assert_eq!(effect("nope"), Ok(json!("unknown")));
        assert!(
            ask(
                &ctx,
                "cli:effect",
                json!({ "id": "nope", "command": "search" })
            )
            .is_err()
        );
    }

    #[test]
    fn 모양이_다른_저장_명령은_버리지_않고_에러로_알린다() {
        let ctx = context();
        seed(
            &ctx,
            json!([{
                "id": "c1", "kind": "cli", "name": "Old",
                "data": { "site": 5, "commands": [{ "name": "half" }], "writes": "yes" },
                "createdAt": 1, "updatedAt": 2
            }]),
        );
        let error = ask(&ctx, "cli:get", json!({ "id": "c1" })).unwrap_err();
        assert!(
            error.starts_with("a saved CLI command is not readable: "),
            "{error}"
        );
        assert!(
            ask(
                &ctx,
                "cli:set-writes",
                json!({ "id": "c1", "writes": true })
            )
            .is_err()
        );
        assert_eq!(
            saved(&ctx)["state"]["workflows"][0]["data"]["commands"][0],
            json!({ "name": "half" })
        );
    }
}
