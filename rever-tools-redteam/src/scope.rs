//! Port of `src/main/mcp/tools/scope.ts` — the `scope_*` tools and `lab_target`.
//!
//! The scope and the lab-target list themselves live in `rever-scope`, process
//! wide, because navigation, replay and the network fence consult the same state.

use rever_scope::fence::{clear_lab_hosts, get_lab_hosts, set_lab_hosts};
use rever_scope::scope::{
    ScopeState, add_scope_hosts, clear_scope, get_scope, hostname_of, set_scope,
};
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Map, Value, json};

use crate::args::{strings, validate};
use crate::{active_url, no_args, pretty, schema};

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ScopeStatus);
    registry.register(ScopeSet);
    registry.register(ScopeAdd);
    registry.register(ScopeClear);
    registry.register(LabTarget);
}

async fn current_target_host(ctx: &ToolContext) -> Option<String> {
    hostname_of(&active_url(ctx).await?)
}

/// The tool result every scope tool ends with. `labHosts` and `note` are left
/// out when they have nothing to say, as `undefined` is by JSON.stringify.
fn scope_state() -> ToolResult {
    let ScopeState { domains, enabled } = get_scope();
    let lab_hosts = get_lab_hosts();
    let mut state = Map::new();
    state.insert("enabled".into(), json!(enabled));
    state.insert("domains".into(), json!(domains));
    if !lab_hosts.is_empty() {
        state.insert("labHosts".into(), json!(lab_hosts));
    }
    if !enabled {
        state.insert("note".into(), json!("No scope set — all hosts allowed."));
    }
    ok(pretty(&state))
}

pub struct ScopeStatus;

impl Tool for ScopeStatus {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "scope_status",
            "Show the active target scope. When enabled, navigate/replay/repeater/intruder/burst refuse hosts outside these registrable domains. Empty = no enforcement (all hosts allowed).",
            no_args(),
        )
    }

    // No argument check: the TypeScript registered this tool without an input
    // schema, so whatever was sent was accepted.
    fn call<'a>(&'a self, _: &'a ToolContext, _: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { scope_state() })
    }
}

pub struct ScopeSet;

impl Tool for ScopeSet {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "scope_set",
            "Set the target scope to the given hosts/URLs (stored as registrable domains, so subdomains like api./cdn. are included). Pass no hosts to scope to the current tab. Replaces any existing scope.",
            schema(
                json!({
                    "hosts": {
                        "description": "Hosts or URLs. Omit to use the current tab host.",
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }),
                &[],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            let mut input = strings(&args, "hosts").unwrap_or_default();
            if input.is_empty() {
                let Some(host) = current_target_host(ctx).await else {
                    return err("no hosts given and no active tab to derive scope from");
                };
                input = vec![host];
            }
            let domains = set_scope(&input);
            if domains.is_empty() {
                return err("no valid hosts parsed from input");
            }
            scope_state()
        })
    }
}

pub struct ScopeAdd;

impl Tool for ScopeAdd {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "scope_add",
            "Add hosts/URLs to the current scope (registrable domains).",
            schema(
                json!({
                    "hosts": {
                        "minItems": 1,
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Hosts or URLs to allow"
                    }
                }),
                &["hosts"],
            ),
        )
    }

    fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            add_scope_hosts(&strings(&args, "hosts").unwrap_or_default());
            scope_state()
        })
    }
}

pub struct ScopeClear;

impl Tool for ScopeClear {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "scope_clear",
            "Clear the scope (disables enforcement — all hosts allowed again).",
            no_args(),
        )
    }

    // No argument check: registered without an input schema, like scope_status.
    fn call<'a>(&'a self, _: &'a ToolContext, _: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            clear_scope();
            scope_state()
        })
    }
}

pub struct LabTarget;

impl Tool for LabTarget {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "lab_target",
            "Mark EXTRA (non-loopback) hosts as disposable LAB targets so the capture-not-send fence steps aside for them: POST/PUT/PATCH/DELETE pass through to the real server instead of being answered with a synthetic 204, letting the red team CONFIRM mutation-only vulns (SSTI, SQLi/cmd-injection via POST) instead of stopping at \"candidate\". Loopback (localhost / 127.x / ::1) is ALREADY a lab target by default, so you only need this for a non-loopback host you own and are authorized to mutate (e.g. an authorized staging domain). Pass no hosts to use the current tab; pass clear:true to drop the extra hosts.",
            schema(
                json!({
                    "hosts": {
                        "description": "Hosts or URLs. Omit to use the current tab host.",
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "clear": {
                        "description": "Clear all lab targets (re-fence every host).",
                        "type": "boolean"
                    }
                }),
                &[],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            if args.get("clear").and_then(Value::as_bool) == Some(true) {
                clear_lab_hosts();
                return scope_state();
            }
            let mut input = strings(&args, "hosts").unwrap_or_default();
            if input.is_empty() {
                let Some(host) = current_target_host(ctx).await else {
                    return err("no hosts given and no active tab to derive a lab target from");
                };
                input = vec![host];
            }
            let lab = set_lab_hosts(&input);
            if lab.is_empty() {
                return err("no valid hosts parsed from input");
            }
            scope_state()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use rever_scope::fence::is_lab_host;
    use rever_scope::scope::scope_block_for_url;

    use super::*;
    use crate::testing::{TempDir, call};

    /// The scope and lab hosts are process-wide and cargo runs tests on parallel
    /// threads, so tests touching them run one at a time and reset on entry and exit.
    struct Isolated(#[allow(dead_code)] MutexGuard<'static, ()>);

    impl Drop for Isolated {
        fn drop(&mut self) {
            clear_scope();
            clear_lab_hosts();
        }
    }

    fn isolated() -> Isolated {
        static SERIAL: Mutex<()> = Mutex::new(());
        let guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        clear_scope();
        clear_lab_hosts();
        Isolated(guard)
    }

    const OFF: &str = r#"{
  "enabled": false,
  "domains": [],
  "note": "No scope set — all hosts allowed."
}"#;

    #[tokio::test]
    async fn scope가_없으면_모든_호스트를_허용한다고_알린다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let ctx = dir.ctx(None);
        // Registered without a schema: any arguments are accepted, even none.
        assert_eq!(
            call(&ScopeStatus, &ctx, Value::Null).await,
            (false, OFF.into())
        );
    }

    #[tokio::test]
    async fn scope_set은_호스트를_등록_도메인으로_접어_기존_scope를_교체한다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let ctx = dir.ctx(None);
        set_scope(&["old.test"]);
        let args =
            json!({ "hosts": ["https://api.example.com/v1", "cdn.example.com", "localhost:8080"] });
        let expected = r#"{
  "enabled": true,
  "domains": [
    "example.com",
    "localhost:8080"
  ]
}"#;
        assert_eq!(call(&ScopeSet, &ctx, args).await, (false, expected.into()));
        assert_eq!(
            call(&ScopeStatus, &ctx, json!({})).await,
            (false, expected.into())
        );
        // The scope the tool set is the one navigation is checked against.
        assert!(scope_block_for_url(Some("https://old.test/")).is_some());
        assert!(scope_block_for_url(Some("https://www.example.com/")).is_none());
    }

    #[tokio::test]
    async fn scope_set에_호스트가_없으면_현재_탭의_호스트를_쓴다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let ctx = dir.ctx(Some("https://shop.example.co.kr/cart?x=1"));
        let expected = r#"{
  "enabled": true,
  "domains": [
    "example.co.kr"
  ]
}"#;
        assert_eq!(
            call(&ScopeSet, &ctx, json!({})).await,
            (false, expected.into())
        );
        // An empty list counts as "no hosts" too.
        clear_scope();
        let args = json!({ "hosts": [] });
        assert_eq!(call(&ScopeSet, &ctx, args).await, (false, expected.into()));
    }

    #[tokio::test]
    async fn scope_set에_호스트도_탭도_없으면_에러다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let no_tab = dir.ctx(None);
        let refusal = "no hosts given and no active tab to derive scope from";
        assert_eq!(
            call(&ScopeSet, &no_tab, json!({})).await,
            (true, refusal.into())
        );
        assert!(!get_scope().enabled);
    }

    #[tokio::test]
    async fn 탭_url에_호스트가_없으면_탭이_없는_것과_같은_에러다() {
        let _serial = isolated();
        let dir = TempDir::new();
        // What the TypeScript answered for a tab at these URLs.
        for url in ["", "file:///x/y"] {
            let ctx = dir.ctx(Some(url));
            assert_eq!(
                call(&ScopeSet, &ctx, json!({})).await,
                (
                    true,
                    "no hosts given and no active tab to derive scope from".into()
                ),
                "{url}"
            );
            assert_eq!(
                call(&LabTarget, &ctx, json!({ "hosts": [] })).await,
                (
                    true,
                    "no hosts given and no active tab to derive a lab target from".into()
                ),
                "{url}"
            );
        }
        assert!(!get_scope().enabled);
        assert!(get_lab_hosts().is_empty());
        // A URL without `//` is read as a bare host, scheme and all, there too.
        let blank = dir.ctx(Some("about:blank"));
        let (is_error, _) = call(&ScopeSet, &blank, json!({})).await;
        assert!(!is_error);
        assert_eq!(get_scope().domains, ["about"]);
    }

    #[tokio::test]
    async fn scope_set은_해석할_호스트가_없으면_에러를_내고_scope는_비워진다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let ctx = dir.ctx(None);
        set_scope(&["old.test"]);
        let args = json!({ "hosts": ["!!!", ""] });
        assert_eq!(
            call(&ScopeSet, &ctx, args).await,
            (true, "no valid hosts parsed from input".into())
        );
        // As in the TypeScript, the replacement has already happened by then.
        assert!(!get_scope().enabled);
    }

    #[tokio::test]
    async fn scope_set은_타입이_틀린_인자를_거부하고_scope를_건드리지_않는다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let ctx = dir.ctx(None);
        set_scope(&["old.test"]);
        let (is_error, text) = call(&ScopeSet, &ctx, json!({ "hosts": "a.test" })).await;
        assert!(is_error);
        assert!(text.starts_with(
            "MCP error -32602: Input validation error: Invalid arguments for tool scope_set: "
        ));
        assert!(text.contains("Invalid input: expected array, received string"));
        assert_eq!(get_scope().domains, ["old.test"]);
    }

    #[tokio::test]
    async fn scope_add는_기존_scope에_중복_없이_더한다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let ctx = dir.ctx(None);
        set_scope(&["example.com"]);
        let args = json!({ "hosts": ["api.example.com", "https://other.test/x", "!!!"] });
        let expected = r#"{
  "enabled": true,
  "domains": [
    "example.com",
    "other.test"
  ]
}"#;
        assert_eq!(call(&ScopeAdd, &ctx, args).await, (false, expected.into()));
    }

    #[tokio::test]
    async fn scope_add는_빈_목록과_누락을_거부한다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let ctx = dir.ctx(Some("https://example.com/"));
        let (is_error, text) = call(&ScopeAdd, &ctx, json!({ "hosts": [] })).await;
        assert!(is_error);
        assert!(text.contains("Too small: expected array to have >=1 items"));
        let (is_error, text) = call(&ScopeAdd, &ctx, json!({})).await;
        assert!(is_error);
        assert!(text.contains("Invalid input: expected array, received undefined"));
        assert!(!get_scope().enabled);
    }

    #[tokio::test]
    async fn scope_clear는_scope를_끄고_lab_호스트는_그대로_둔다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let ctx = dir.ctx(None);
        set_scope(&["example.com"]);
        set_lab_hosts(&["staging.example.com"]);
        let expected = r#"{
  "enabled": false,
  "domains": [],
  "labHosts": [
    "example.com"
  ],
  "note": "No scope set — all hosts allowed."
}"#;
        assert_eq!(
            call(&ScopeClear, &ctx, Value::Null).await,
            (false, expected.into())
        );
    }

    #[tokio::test]
    async fn lab_target은_호스트를_lab으로_표시하고_상태에_보여준다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let ctx = dir.ctx(None);
        let args = json!({ "hosts": ["https://staging.example.com/app", "10.0.0.5:8443"] });
        let expected = r#"{
  "enabled": false,
  "domains": [],
  "labHosts": [
    "example.com",
    "10.0.0.5:8443"
  ],
  "note": "No scope set — all hosts allowed."
}"#;
        assert_eq!(call(&LabTarget, &ctx, args).await, (false, expected.into()));
        // The fence reads the same list.
        assert!(is_lab_host("https://api.example.com/x"));
        assert!(is_lab_host("http://10.0.0.5:8443/x"));
        assert!(!is_lab_host("http://10.0.0.5:9000/x"));
        assert!(!is_lab_host("https://other.test/"));
    }

    #[tokio::test]
    async fn lab_target에_호스트가_없으면_현재_탭을_쓰고_탭도_없으면_에러다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let ctx = dir.ctx(Some("https://staging.example.com/app"));
        let (is_error, _) = call(&LabTarget, &ctx, json!({})).await;
        assert!(!is_error);
        assert_eq!(get_lab_hosts(), ["example.com"]);

        clear_lab_hosts();
        let no_tab = dir.ctx(None);
        assert_eq!(
            call(&LabTarget, &no_tab, json!({ "clear": false })).await,
            (
                true,
                "no hosts given and no active tab to derive a lab target from".into()
            )
        );
        assert!(get_lab_hosts().is_empty());
    }

    #[tokio::test]
    async fn lab_target은_해석할_호스트가_없으면_에러를_내고_목록은_비워진다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let ctx = dir.ctx(None);
        set_lab_hosts(&["staging.example.com"]);
        assert_eq!(
            call(&LabTarget, &ctx, json!({ "hosts": ["!!!"] })).await,
            (true, "no valid hosts parsed from input".into())
        );
        assert!(get_lab_hosts().is_empty());
    }

    #[tokio::test]
    async fn lab_target의_clear는_hosts보다_먼저이고_모든_lab_호스트를_지운다() {
        let _serial = isolated();
        let dir = TempDir::new();
        let ctx = dir.ctx(None);
        set_lab_hosts(&["staging.example.com"]);
        let args = json!({ "clear": true, "hosts": ["other.test"] });
        assert_eq!(call(&LabTarget, &ctx, args).await, (false, OFF.into()));
        assert!(get_lab_hosts().is_empty());
        assert!(!is_lab_host("https://staging.example.com/"));
    }
}
