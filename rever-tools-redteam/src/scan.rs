//! Port of `src/main/mcp/tools/scan.ts` — `redteam_start`.
//!
//! Launch the deterministic Red Team pipeline (the one behind the Red Team
//! popup): discovery → parallel per-tab worker agents → optional probe phase →
//! merged report. This is the ONLY way one agent command covers dozens of tabs
//! — hand-orchestrating sub-agents per tab collides on the shared browser, so
//! steer here instead of spawning Task sub-agents for site-wide sweeps.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rever_scope::fence::is_loopback_host;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Value, json};

use crate::args::{text, validate};
use crate::schema;

pub fn register(registry: &mut ToolRegistry) {
    registry.register(RedteamStart);
}

type Handler = Arc<dyn Fn(&str, Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

/// Stand-in for the Electron renderer bridge (`askRenderer`): the scan runs in
/// the interface, so the tool hands it a request and waits for the answer.
/// Held in `ctx.state::<RendererBridge>()`; until the app subscribes nothing
/// listens and every request fails like the TypeScript did with no window open.
#[derive(Default)]
pub struct RendererBridge(Mutex<Option<Handler>>);

impl RendererBridge {
    /// The interface answers requests here: `handler(op, payload)` resolves to
    /// the reply, or to the error message to report. Replaces an earlier handler.
    pub fn subscribe(
        &self,
        handler: impl Fn(&str, Value) -> BoxFuture<'static, Result<Value, String>>
        + Send
        + Sync
        + 'static,
    ) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(handler));
    }

    /// Needs a tokio runtime with timers once a handler is subscribed.
    async fn ask(&self, op: &str, payload: Value, timeout_ms: u64) -> Result<Value, String> {
        let handler = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(handler) = handler else {
            return Err("no renderer window".to_owned());
        };
        let answer = handler(op, payload);
        match tokio::time::timeout(Duration::from_millis(timeout_ms), answer).await {
            Ok(answer) => answer,
            Err(_) => Err(format!(
                "renderer did not answer \"{op}\" in {timeout_ms}ms"
            )),
        }
    }
}

/// `/^https?:\/\//i`
fn is_absolute_http(url: &str) -> bool {
    let head: String = url.chars().take(8).collect::<String>().to_ascii_lowercase();
    head.starts_with("http://") || head.starts_with("https://")
}

/// `String(value)`, which is how a template literal prints a value.
fn js_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => {
            // Array.prototype.join prints null as nothing.
            let print = |item: &Value| {
                if item.is_null() {
                    String::new()
                } else {
                    js_string(item)
                }
            };
            items.iter().map(print).collect::<Vec<_>>().join(",")
        }
        Value::Object(_) => "[object Object]".to_owned(),
        // A whole float prints without the ".0" serde adds, and -0 as "0".
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 9e15 => (f as i64).to_string(),
            _ => n.to_string(),
        },
        other => other.to_string(),
    }
}

/// `res?.phase ?? fallback`, as a template literal prints it.
fn phase_of(reply: &Value, fallback: &str) -> String {
    match reply.get("phase") {
        None | Some(Value::Null) => fallback.to_owned(),
        Some(phase) => js_string(phase),
    }
}

pub struct RedteamStart;

impl Tool for RedteamStart {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "redteam_start",
            "Launch a site-wide Red Team scan of one target URL: opens up to N same-site pages as background tabs, runs one inspecting agent per tab in parallel (read + dynamic exploration), then (at probe/redteam intensity) single-shot probes against the deduped endpoint set, and finishes with a merged Markdown report. By default the user confirms the run in the Red Team popup — active intensities are never started silently. Pass start:true to run the whole pipeline without a click; this is honoured ONLY for loopback targets (localhost/127.x/::1 — your own machine), matching the network fence, so the agent can self-drive local benchmarks. Non-loopback targets ignore start:true and still wait for the human click. Use this instead of spawning sub-agents per tab. Poll progress with finding_list / list_requests; the final report is exported by finding_export.",
            schema(
                json!({
                    "url": {
                        "type": "string",
                        "description": "Absolute start URL of the site to scan (scheme required)"
                    },
                    "pages": {
                        "description": "Page budget, 1-200 (default 20)",
                        "type": "integer",
                        "minimum": -9_007_199_254_740_991_i64,
                        "maximum": 9_007_199_254_740_991_i64
                    },
                    "intensity": {
                        "description": "readonly = observe only; probe = replay captured requests unchanged + security headers; redteam = additionally one mutated/payload request per endpoint (default readonly)",
                        "type": "string",
                        "enum": ["readonly", "probe", "redteam"]
                    },
                    "start": {
                        "description": "Run the full pipeline immediately without the user click. Honoured only for loopback targets (localhost/127.x/::1); ignored otherwise.",
                        "type": "boolean"
                    }
                }),
                &["url"],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            let url = text(&args, "url").unwrap_or_default();
            if !is_absolute_http(url) {
                return err("url must be absolute, including the scheme");
            }
            let start = args.get("start").and_then(Value::as_bool) == Some(true);
            let autostart = start && is_loopback_host(url);
            let intensity = text(&args, "intensity").unwrap_or("readonly");
            let payload = json!({
                "url": url,
                // As an integer: JSON.stringify prints 5.0 as 5.
                "pages": args.get("pages").and_then(Value::as_f64).map_or(20, |n| n as i64),
                "intensity": intensity,
                "autostart": autostart,
            });
            // autostart opens the target tab and waits for its webview to attach
            // (up to 10s) before returning, so allow more than the 8s default.
            let timeout_ms = if autostart { 15000 } else { 8000 };
            let bridge = ctx.state::<RendererBridge>();
            let reply = match bridge.ask("scan:launch", payload, timeout_ms).await {
                Ok(reply) => reply,
                Err(error) => return err(format!("could not arm the scan: {error}")),
            };
            if autostart {
                return ok(format!(
                    "scan started ({intensity}, loopback target) — running without a click. \
                     Findings appear via finding_add (poll finding_list), and the merged report is produced by finding_export when it finishes. Reply state: {}",
                    phase_of(&reply, "running")
                ));
            }
            let why = if start {
                " (start:true ignored — target is not loopback, so a human click is required)"
            } else {
                ""
            };
            ok(format!(
                "scan armed ({intensity}) — the user confirms it in the Red Team popup; nothing runs until then{why}. \
                 Once running, findings appear via finding_add (poll finding_list) and the merged report is produced by finding_export at the end. Reply state: {}",
                phase_of(&reply, "confirm")
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use rever_tools_api::utils::Content;

    use super::*;

    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    /// A context whose interface answers every request with `reply` and records it.
    fn listening(reply: Result<Value, String>) -> (ToolContext, Seen) {
        let ctx = ToolContext::for_tests();
        let seen = Seen::default();
        let log = seen.clone();
        ctx.state::<RendererBridge>().subscribe(move |op, payload| {
            log.lock().unwrap().push((op.to_owned(), payload));
            let reply = reply.clone();
            Box::pin(async move { reply })
        });
        (ctx, seen)
    }

    async fn run(ctx: &ToolContext, args: Value) -> (bool, String) {
        let result = RedteamStart.call(ctx, args).await;
        let text = match result.content.first() {
            Some(Content::Text { text }) => text.clone(),
            None => String::new(),
        };
        (result.is_error, text)
    }

    #[tokio::test]
    async fn 스킴이_없는_url은_인터페이스에_묻기_전에_거부한다() {
        let (ctx, seen) = listening(Ok(json!({})));
        for url in [
            "example.com",
            "//example.com",
            "ftp://example.com",
            " http://a.test",
            "",
        ] {
            assert_eq!(
                run(&ctx, json!({ "url": url })).await,
                (true, "url must be absolute, including the scheme".into()),
                "{url}"
            );
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn 듣는_인터페이스가_없으면_창이_없다는_에러다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            run(
                &ctx,
                json!({ "url": "http://localhost:1337", "start": true })
            )
            .await,
            (true, "could not arm the scan: no renderer window".into())
        );
    }

    #[tokio::test]
    async fn 기본값으로_무장하고_사용자_확인을_기다린다() {
        let (ctx, seen) = listening(Ok(json!({ "phase": "confirm" })));
        let (is_error, text) = run(&ctx, json!({ "url": "HTTPS://Example.com/app" })).await;
        assert!(!is_error);
        assert_eq!(
            text,
            "scan armed (readonly) — the user confirms it in the Red Team popup; nothing runs until then. Once running, findings appear via finding_add (poll finding_list) and the merged report is produced by finding_export at the end. Reply state: confirm"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "scan:launch");
        assert_eq!(
            serde_json::to_string(&seen[0].1).unwrap(),
            r#"{"url":"HTTPS://Example.com/app","pages":20,"intensity":"readonly","autostart":false}"#
        );
    }

    #[tokio::test]
    async fn 루프백이_아니면_start_true를_무시하고_이유를_알린다() {
        let (ctx, seen) = listening(Ok(Value::Null));
        let args = json!({
            "url": "https://example.com/", "pages": 50, "intensity": "redteam", "start": true
        });
        let (is_error, text) = run(&ctx, args).await;
        assert!(!is_error);
        assert_eq!(
            text,
            "scan armed (redteam) — the user confirms it in the Red Team popup; nothing runs until then (start:true ignored — target is not loopback, so a human click is required). Once running, findings appear via finding_add (poll finding_list) and the merged report is produced by finding_export at the end. Reply state: confirm"
        );
        assert_eq!(
            seen.lock().unwrap()[0].1,
            json!({ "url": "https://example.com/", "pages": 50, "intensity": "redteam", "autostart": false })
        );
    }

    #[tokio::test]
    async fn 루프백_대상은_start_true로_클릭_없이_시작한다() {
        for url in [
            "http://localhost:1337/",
            "http://127.0.0.1:8080",
            "http://[::1]:3000/x",
        ] {
            let (ctx, seen) = listening(Ok(json!({ "phase": "discovering" })));
            let args = json!({ "url": url, "intensity": "probe", "start": true });
            let (is_error, text) = run(&ctx, args).await;
            assert!(!is_error, "{url}");
            assert_eq!(
                text,
                "scan started (probe, loopback target) — running without a click. Findings appear via finding_add (poll finding_list), and the merged report is produced by finding_export when it finishes. Reply state: discovering"
            );
            assert_eq!(seen.lock().unwrap()[0].1["autostart"], true, "{url}");
        }
    }

    #[tokio::test]
    async fn 루프백이어도_start가_없으면_자동으로_시작하지_않는다() {
        let (ctx, seen) = listening(Ok(json!({})));
        let (_, text) = run(
            &ctx,
            json!({ "url": "http://localhost:1337/", "start": false }),
        )
        .await;
        assert!(text.starts_with("scan armed (readonly) — "));
        assert!(!text.contains("start:true ignored"));
        assert_eq!(seen.lock().unwrap()[0].1["autostart"], false);
    }

    #[tokio::test]
    async fn 루프백처럼_보이는_공개_호스트는_자동으로_시작하지_않는다() {
        // The loopback check is the fence's: only the URL's own host counts.
        for url in [
            "http://localhost.example.com/",
            "http://example.com/?next=http://localhost/",
            "http://localhost@example.com/",
            "http://128.0.0.1/",
        ] {
            let (ctx, seen) = listening(Ok(json!({})));
            let (_, text) = run(&ctx, json!({ "url": url, "start": true })).await;
            assert!(text.contains("start:true ignored"), "{url}");
            assert_eq!(seen.lock().unwrap()[0].1["autostart"], false, "{url}");
        }
    }

    #[tokio::test]
    async fn 응답에_phase가_없으면_기본_상태를_쓴다() {
        let (ctx, _) = listening(Ok(json!({ "ok": true })));
        let (_, text) = run(&ctx, json!({ "url": "http://localhost/", "start": true })).await;
        assert!(text.ends_with("Reply state: running"));
        let (_, text) = run(&ctx, json!({ "url": "http://localhost/" })).await;
        assert!(text.ends_with("Reply state: confirm"));
    }

    #[tokio::test]
    async fn 소수점으로_온_정수_pages는_정수로_전달한다() {
        let (ctx, seen) = listening(Ok(json!({})));
        for pages in [json!(5.0), json!(1e3), json!(-0.0), json!(-3)] {
            run(&ctx, json!({ "url": "http://a.test/", "pages": pages })).await;
        }
        let sent: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, payload)| payload["pages"].to_string())
            .collect();
        // What JSON.stringify printed for the same arguments in the TypeScript.
        assert_eq!(sent, ["5", "1000", "0", "-3"]);
    }

    #[tokio::test]
    async fn 문자열이_아닌_phase는_자바스크립트처럼_출력한다() {
        // Each expectation is what the TypeScript template literal printed.
        for (phase, printed) in [
            (json!(5), "5"),
            (json!(1.0), "1"),
            (json!(-0.0), "0"),
            (json!(-7.0), "-7"),
            (json!(1.5), "1.5"),
            (json!(true), "true"),
            (json!(""), ""),
            (json!(["a", "b"]), "a,b"),
            (json!([null, ["x", 1], {}]), ",x,1,[object Object]"),
            (json!({ "a": 1 }), "[object Object]"),
        ] {
            let (ctx, _) = listening(Ok(json!({ "phase": phase })));
            let (_, text) = run(&ctx, json!({ "url": "http://a.test/" })).await;
            assert!(text.ends_with(&format!("Reply state: {printed}")), "{text}");
        }
        // A reply that is not an object has no phase.
        for reply in [json!("str"), json!([1]), json!(0), Value::Null] {
            let (ctx, _) = listening(Ok(reply));
            let (_, text) = run(&ctx, json!({ "url": "http://a.test/" })).await;
            assert!(text.ends_with("Reply state: confirm"), "{text}");
        }
    }

    #[tokio::test]
    async fn 인터페이스가_에러로_답하면_그_메시지를_전한다() {
        let (ctx, _) = listening(Err("scan already running".into()));
        assert_eq!(
            run(&ctx, json!({ "url": "https://example.com/" })).await,
            (true, "could not arm the scan: scan already running".into())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 답이_없으면_8초_자동_시작은_15초_뒤에_포기한다() {
        let ctx = ToolContext::for_tests();
        ctx.state::<RendererBridge>()
            .subscribe(|_, _| Box::pin(std::future::pending()));

        let started = tokio::time::Instant::now();
        assert_eq!(
            run(&ctx, json!({ "url": "https://example.com/" })).await,
            (
                true,
                "could not arm the scan: renderer did not answer \"scan:launch\" in 8000ms".into()
            )
        );
        assert_eq!(started.elapsed(), Duration::from_millis(8000));

        let started = tokio::time::Instant::now();
        assert_eq!(
            run(
                &ctx,
                json!({ "url": "http://localhost:1337/", "start": true })
            )
            .await,
            (
                true,
                "could not arm the scan: renderer did not answer \"scan:launch\" in 15000ms".into()
            )
        );
        assert_eq!(started.elapsed(), Duration::from_millis(15000));
    }

    #[tokio::test]
    async fn 잘못된_인자는_인터페이스에_묻기_전에_거부한다() {
        let (ctx, seen) = listening(Ok(json!({})));
        let args = json!({ "url": "https://example.com/", "pages": 1.5, "intensity": "loud" });
        let (is_error, text) = run(&ctx, args).await;
        assert!(is_error);
        assert!(text.starts_with(
            "MCP error -32602: Input validation error: Invalid arguments for tool redteam_start: "
        ));
        assert!(text.contains("Invalid input: expected int, received number"));
        assert!(
            text.contains(r#"Invalid option: expected one of \"readonly\"|\"probe\"|\"redteam\""#)
        );
        let (is_error, _) = run(&ctx, json!({})).await;
        assert!(is_error);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn 스키마는_zod가_만든_것과_같다() {
        let schema = RedteamStart.def().input_schema;
        assert_eq!(schema["required"], json!(["url"]));
        assert_eq!(
            serde_json::to_string(&schema["properties"]["pages"]).unwrap(),
            r#"{"description":"Page budget, 1-200 (default 20)","type":"integer","minimum":-9007199254740991,"maximum":9007199254740991}"#
        );
    }
}
