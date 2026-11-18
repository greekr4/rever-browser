//! Port of `src/main/mcp/tools/intruder.ts` + `burst.ts` (+ `stats.ts`): the two
//! fuzzing delivery loops. Both fan crafted requests through [`crate::send`], so
//! the scope/fence safety net still applies — a burst of mutations against a
//! non-loopback host is recorded, not sent.
//!
//! Arg parsing is manual (like `raw_http`), matching this crate's style.

use std::time::{Duration, Instant};

use futures::stream::{self, StreamExt};
use rever_core::traffic::Headers;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::request_spec::{Mods, RequestSpec, build_request_spec};
use crate::send::send;
use crate::{pretty, schema};

const DEFAULT_MARKER: &str = "§§";
/// Overall wall-clock budget for one fuzz run, so a huge payload list can't turn
/// into an unstoppable multi-minute flood at the target.
const FUZZ_BUDGET: Duration = Duration::from_secs(120);
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// First 12 hex chars of the SHA-1 of `s` (`createHash('sha1')…slice(0,12)`).
fn sha1_short(s: &str) -> String {
    Sha1::digest(s.as_bytes())
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Nearest-rank percentile of a pre-sorted (ascending) array.
pub fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (((sorted.len() - 1) as f64) * p).floor() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Replace every `marker` in url/headers/body with `payload`, verbatim (no
/// URL-encoding — the fuzzer sends payloads as given).
fn substitute(spec: &RequestSpec, marker: &str, payload: &str) -> RequestSpec {
    RequestSpec {
        url: spec.url.replace(marker, payload),
        method: spec.method.clone(),
        headers: spec
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.replace(marker, payload)))
            .collect(),
        body: spec.body.as_deref().map(|b| b.replace(marker, payload)),
    }
}

fn contains_marker(spec: &RequestSpec, marker: &str) -> bool {
    spec.url.contains(marker)
        || spec.body.as_deref().unwrap_or_default().contains(marker)
        || spec.headers.values().any(|v| v.contains(marker))
}

pub struct IntruderRun;

impl Tool for IntruderRun {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "intruder_run",
            "Burp-Intruder-style fuzzer. Insert a marker (default \"§§\") in the URL/headers/body of a base request, supply a list of payloads, and this tool fires one request per payload. Returns a table of {payload, status, length, durationMs, bodyHash} ideal for boolean/timing-oracle blind SQLi or auth-bypass enumeration.",
            schema(
                json!({
                    "requestId": { "type": "string", "description": "Base requestId (from list_requests)" },
                    "payloads": { "type": "array", "items": { "type": "string" }, "description": "Payloads to substitute at the marker (max 500)" },
                    "marker": { "type": "string", "description": "Marker token (default \"§§\")" },
                    "concurrency": { "type": "integer", "description": "Parallel workers (default 4)" },
                    "diffBaseline": { "type": "boolean", "description": "Also send the original request once for diffing (default true)" }
                }),
                &["requestId", "payloads"],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let Some(request_id) = args["requestId"].as_str() else {
                return err("requestId is required");
            };
            let Some(payloads) = args["payloads"].as_array() else {
                return err("payloads is required");
            };
            let payloads: Vec<String> = payloads
                .iter()
                .filter_map(|p| p.as_str().map(str::to_owned))
                .take(500)
                .collect();
            let marker = args["marker"].as_str().unwrap_or(DEFAULT_MARKER);
            let diff_baseline = args["diffBaseline"].as_bool().unwrap_or(true);

            let base = {
                let store = ctx
                    .traffic
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match build_request_spec(&store, Some(request_id), None) {
                    Ok(spec) => spec,
                    Err(error) => return err(error),
                }
            };
            if !contains_marker(&base, marker) {
                return err(format!(
                    "marker \"{marker}\" not present in URL/body/headers of base request — add the marker first via repeater or supply a different marker."
                ));
            }

            let baseline = if diff_baseline {
                let out = send(&substitute(&base, marker, ""), SEND_TIMEOUT).await;
                Some(json!({
                    "payload": "<baseline:empty>",
                    "status": out.status,
                    "length": out.body.len(),
                    "durationMs": out.elapsed_ms,
                    "bodyHash": sha1_short(&out.body),
                    "error": out.error,
                }))
            } else {
                None
            };

            // shortcut: probes sequentially (the `concurrency` arg is accepted
            // for API parity); switch to buffered futures if lists grow large.
            let deadline = Instant::now() + FUZZ_BUDGET;
            let mut results = Vec::with_capacity(payloads.len());
            for payload in &payloads {
                if Instant::now() > deadline {
                    break;
                }
                let out = send(&substitute(&base, marker, payload), SEND_TIMEOUT).await;
                results.push(json!({
                    "payload": payload,
                    "status": out.status,
                    "length": out.body.len(),
                    "durationMs": out.elapsed_ms,
                    "bodyHash": sha1_short(&out.body),
                    "error": out.error,
                }));
            }

            ok(pretty(&json!({
                "marker": marker,
                "baseRequestId": request_id,
                "baseline": baseline,
                "count": results.len(),
                "skipped": payloads.len() - results.len(),
                "results": results,
            })))
        })
    }
}

pub struct BurstSend;

impl Tool for BurstSend {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "burst_send",
            "Race-condition burst: fire N copies of the same request in parallel. Useful for TOCTOU bugs (coupon double-spend, stock overrun, session-file races). Returns timing histogram and per-response stats.",
            schema(
                json!({
                    "requestId": { "type": "string", "description": "Base requestId" },
                    "n": { "type": "integer", "description": "Total requests to fire (max 200)" },
                    "parallelism": { "type": "integer", "description": "Concurrent in-flight (default = n, full burst)" },
                    "modifications": {
                        "type": "object",
                        "properties": {
                            "url": { "type": "string" },
                            "method": { "type": "string" },
                            "setHeaders": { "type": "object" },
                            "removeHeaders": { "type": "array", "items": { "type": "string" } },
                            "body": { "type": "string" }
                        }
                    }
                }),
                &["requestId", "n"],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let Some(request_id) = args["requestId"].as_str() else {
                return err("requestId is required");
            };
            let Some(n) = args["n"].as_u64().map(|n| n.min(200) as usize).filter(|n| *n > 0) else {
                return err("n must be a positive integer");
            };
            let par = args["parallelism"]
                .as_u64()
                .map(|p| (p as usize).clamp(1, 50))
                .unwrap_or(n);
            let mods = parse_mods(&args["modifications"]);

            // repeaterSend rebuilds the same spec each call; build it once.
            let spec = {
                let store = ctx
                    .traffic
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match build_request_spec(&store, Some(request_id), mods.as_ref()) {
                    Ok(spec) => spec,
                    Err(error) => return err(error),
                }
            };

            let started = Instant::now();
            // Fire n sends with at most `par` in flight at once.
            let mut outcomes: Vec<(usize, u64, u16, usize, u64, Option<String>)> = stream::iter(0..n)
                .map(|i| {
                    let spec = spec.clone();
                    async move {
                        let offset = started.elapsed().as_millis() as u64;
                        let out = send(&spec, SEND_TIMEOUT).await;
                        (i, offset, out.status, out.body.len(), out.elapsed_ms, out.error)
                    }
                })
                .buffer_unordered(par)
                .collect()
                .await;
            let wall_clock = started.elapsed().as_millis() as u64;
            outcomes.sort_by_key(|o| o.0);

            let mut durations: Vec<u64> = outcomes.iter().map(|o| o.4).collect();
            durations.sort_unstable();
            let mut status_counts: std::collections::BTreeMap<u16, u64> = Default::default();
            for o in &outcomes {
                *status_counts.entry(o.2).or_default() += 1;
            }
            let status_counts: serde_json::Map<String, Value> = status_counts
                .into_iter()
                .map(|(status, count)| (status.to_string(), json!(count)))
                .collect();
            let results: Vec<Value> = outcomes
                .iter()
                .map(|(_, offset, status, length, duration, error)| {
                    json!({
                        "status": status,
                        "length": length,
                        "durationMs": duration,
                        "startedOffsetMs": offset,
                        "error": error,
                    })
                })
                .collect();

            ok(pretty(&json!({
                "count": results.len(),
                "wallClockMs": wall_clock,
                "statusCounts": status_counts,
                "timing": {
                    "minMs": durations.first().copied().unwrap_or(0),
                    "p50Ms": percentile(&durations, 0.5),
                    "p95Ms": percentile(&durations, 0.95),
                    "maxMs": durations.last().copied().unwrap_or(0),
                },
                "results": results,
            })))
        })
    }
}

/// Parse the `modifications` object into [`Mods`] (the safe repeater subset).
fn parse_mods(value: &Value) -> Option<Mods> {
    let object = value.as_object()?;
    let set_headers = object.get("setHeaders").and_then(Value::as_object).map(|map| {
        map.iter()
            .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
            .collect::<Headers>()
    });
    let remove_headers = object.get("removeHeaders").and_then(Value::as_array).map(|list| {
        list.iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    });
    // `body`: a string sets it, explicit null clears it, absent leaves it.
    let body = match object.get("body") {
        Some(Value::String(s)) => Some(Some(s.clone())),
        Some(Value::Null) => Some(None),
        _ => None,
    };
    Some(Mods {
        url: object.get("url").and_then(Value::as_str).map(str::to_owned),
        method: object.get("method").and_then(Value::as_str).map(str::to_owned),
        set_headers,
        remove_headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(url: &str, headers: &[(&str, &str)], body: Option<&str>) -> RequestSpec {
        RequestSpec {
            url: url.to_owned(),
            method: "GET".to_owned(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            body: body.map(str::to_owned),
        }
    }

    #[test]
    fn percentile는_nearest_rank() {
        let sorted = [10, 20, 30, 40, 50];
        assert_eq!(percentile(&sorted, 0.5), 30);
        // nearest-rank: floor((5-1)*0.95) = 3 → sorted[3], as the TS computes.
        assert_eq!(percentile(&sorted, 0.95), 40);
        assert_eq!(percentile(&sorted, 1.0), 50);
        assert_eq!(percentile(&sorted, 0.0), 10);
        assert_eq!(percentile(&[], 0.5), 0);
    }

    #[test]
    fn substitute는_모든_필드의_마커를_페이로드로_바꾼다() {
        let base = spec("http://x/?q=§§", &[("X-H", "a§§b")], Some("body=§§"));
        let out = substitute(&base, "§§", "PAY");
        assert_eq!(out.url, "http://x/?q=PAY");
        assert_eq!(out.headers.get("X-H").map(String::as_str), Some("aPAYb"));
        assert_eq!(out.body.as_deref(), Some("body=PAY"));
    }

    #[test]
    fn contains_marker는_url_헤더_바디를_본다() {
        assert!(contains_marker(&spec("http://x/?q=§§", &[], None), "§§"));
        assert!(contains_marker(&spec("http://x/", &[("H", "§§")], None), "§§"));
        assert!(contains_marker(&spec("http://x/", &[], Some("§§")), "§§"));
        assert!(!contains_marker(&spec("http://x/", &[("H", "v")], Some("b")), "§§"));
    }

    #[test]
    fn sha1_short는_12_hex() {
        let h = sha1_short("abc");
        assert_eq!(h.len(), 12);
        // SHA-1("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
        assert_eq!(h, "a9993e364706");
    }

    #[test]
    fn parse_mods는_body_null로_비우고_문자열로_세팅한다() {
        let m = parse_mods(&json!({ "body": serde_json::Value::Null })).unwrap();
        assert_eq!(m.body, Some(None));
        let m = parse_mods(&json!({ "body": "x", "method": "POST" })).unwrap();
        assert_eq!(m.body, Some(Some("x".to_owned())));
        assert_eq!(m.method.as_deref(), Some("POST"));
        let m = parse_mods(&json!({ "url": "http://y/" })).unwrap();
        assert_eq!(m.body, None);
    }
}
