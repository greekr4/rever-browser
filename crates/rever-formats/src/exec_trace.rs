//! Port of `src/main/mcp/exec-trace.ts` — pure helpers for exec_trace.
//!
//! Idea adapted from morluto/rea's observe_web_execution (MIT): V8 precise
//! coverage between two points in time tells which functions actually ran
//! while the user (or agent) did something — a much smaller haystack than a
//! multi-megabyte bundle when hunting for request-signing code.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// Offsets, lines and counts are i64 like the CDP integers they come from, and
// the arithmetic on them saturates: a nonsense position must not panic.

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageRange {
    pub start_offset: i64,
    pub end_offset: i64,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCoverage {
    pub function_name: String,
    pub is_block_coverage: bool,
    pub ranges: Vec<CoverageRange>,
}

/// The CDP Profiler.ScriptCoverage fields this tool reads.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptCoverage {
    pub script_id: String,
    pub url: String,
    pub functions: Vec<FunctionCoverage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutedFunction {
    pub name: String,
    /// UTF-16 offsets into the script source, end exclusive.
    pub start: i64,
    pub end: i64,
    /// Calls during the window (V8 call count of the function's own range).
    pub calls: i64,
    /// False = function-granularity only; branch execution inside is unknown.
    pub block_coverage: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub function_name: String,
    pub url: String,
    pub line_number: i64,
    pub column_number: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceRequest {
    pub request_id: String,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub initiator_stack: Option<Vec<StackFrame>>,
}

#[derive(Clone, Debug, Default)]
pub struct ScriptInfo {
    pub url: String,
    pub source: Option<String>,
    /// traffic-store requestId of the script body, for resolve_source.
    pub request_id: Option<String>,
}

pub const EXEC_TRACE_LIMITATIONS: [&str; 5] = [
    "Coverage disables V8 optimisation while recording, so the page runs slower and timing-sensitive code may behave differently.",
    "A function missing from the result is \"unknown\", not \"did not run\": already-compiled code may report function-level counts only (blockCoverage=false).",
    "URL-less scripts (eval / new Function, and rever's own injected helpers) are hidden unless includeAnonymous=true — turn it on for eval-heavy obfuscated pages.",
    "Main page only — workers, service workers, cross-origin iframes and WebAssembly internals are not covered.",
    "Co-occurrence is not causation: a function that ran during the window did not necessarily produce a given request. onRequestStack is the stronger signal.",
];

/// Scripts worth showing. Electron internals, extensions and DevTools never
/// belong to the page. URL-less scripts are mostly rever's own injected code
/// (cursor HUD, click machinery) — but a page's eval / new Function code is
/// URL-less too, so they're opt-in rather than dropped outright.
pub fn is_traceable_script(url: &str, include_anonymous: bool) -> bool {
    if url.is_empty() {
        return include_anonymous;
    }
    ![
        "node:",
        "chrome-extension:",
        "devtools:",
        "chrome:",
        "extensions:",
    ]
    .iter()
    .any(|scheme| url.starts_with(scheme))
}

/// UTF-16 offset at which each line starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineIndex {
    starts: Vec<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Position {
    pub line: i64,
    pub column: i64,
}

pub fn line_index(source: &str) -> LineIndex {
    let mut starts = vec![0];
    let mut offset: i64 = 0;
    for unit in source.encode_utf16() {
        offset += 1;
        if unit == 10 {
            starts.push(offset);
        }
    }
    LineIndex { starts }
}

/// 1-based line and column of a UTF-16 offset.
pub fn position_at(idx: &LineIndex, offset: i64) -> Position {
    // The last line starting at or before `offset`; the first line when none does.
    let lo = idx
        .starts
        .partition_point(|&start| start <= offset)
        .saturating_sub(1);
    let start = idx.starts.get(lo).copied().unwrap_or(0);
    Position {
        line: i64::try_from(lo).unwrap_or(i64::MAX).saturating_add(1),
        column: offset.saturating_sub(start).saturating_add(1),
    }
}

/// Offset of a 0-based (line, column), as CDP stack frames report them.
pub fn offset_at(idx: &LineIndex, line0: i64, column0: i64) -> Option<i64> {
    let start = idx.starts.get(usize::try_from(line0).ok()?)?;
    Some(start.saturating_add(column0))
}

pub fn executed_functions(script: &ScriptCoverage) -> Vec<ExecutedFunction> {
    let mut out = Vec::new();
    for function in &script.functions {
        let Some(own) = function.ranges.first() else {
            continue;
        };
        if own.count <= 0 {
            continue;
        }
        // The script's top-level body: anonymous and starting at 0. It "runs"
        // only when the script loads, which says nothing about the action.
        if function.function_name.is_empty() && own.start_offset == 0 {
            continue;
        }
        out.push(ExecutedFunction {
            name: function.function_name.clone(),
            start: own.start_offset,
            end: own.end_offset,
            calls: own.count,
            block_coverage: function.is_block_coverage,
        });
    }
    out.sort_by_key(|f| f.start);
    out
}

#[derive(Clone, Copy, Debug)]
pub struct TraceInput<'a> {
    pub coverage: &'a [ScriptCoverage],
    /// By scriptId.
    pub scripts: &'a HashMap<String, ScriptInfo>,
    pub requests: &'a [TraceRequest],
    pub limit: usize,
}

/// The arguments resolve_source takes: 0-based line and column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveSource {
    pub request_id: String,
    pub line: i64,
    pub column: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TracedFunction {
    pub name: String,
    pub location: String,
    pub calls: i64,
    pub block_coverage: bool,
    pub on_request_stack: Vec<String>,
    pub resolve_source: Option<ResolveSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TracedScript {
    pub url: String,
    pub functions: Vec<TracedFunction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TracedRequest {
    pub request_id: String,
    pub method: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceSummary {
    pub total_functions: usize,
    pub omitted_functions: usize,
    pub scripts: Vec<TracedScript>,
    pub requests: Vec<TracedRequest>,
    pub limitations: &'static [&'static str],
}

pub fn summarize_trace(input: TraceInput<'_>) -> TraceSummary {
    let mut budget = input.limit;
    let mut total = 0;
    let mut scripts = Vec::new();
    for cov in input.coverage {
        let fns = executed_functions(cov);
        if fns.is_empty() {
            continue;
        }
        total += fns.len();
        let info = input.scripts.get(&cov.script_id);
        let url = [info.map(|i| i.url.as_str()), Some(cov.url.as_str())]
            .into_iter()
            .flatten()
            .find(|u| !u.is_empty())
            .map_or_else(|| format!("script#{}", cov.script_id), str::to_owned);
        let idx = info
            .and_then(|i| i.source.as_deref())
            .filter(|source| !source.is_empty())
            .map(line_index);

        // Offsets of request call sites that sit in this script.
        let mut call_sites: Vec<(&str, i64)> = Vec::new();
        if let Some(idx) = &idx {
            for r in input.requests {
                for f in r.initiator_stack.iter().flatten() {
                    if f.url != url {
                        continue;
                    }
                    if let Some(off) = offset_at(idx, f.line_number, f.column_number) {
                        call_sites.push((&r.request_id, off));
                    }
                }
            }
        }

        let kept: Vec<&ExecutedFunction> = fns.iter().take(budget).collect();
        budget -= kept.len();
        if kept.is_empty() {
            continue;
        }
        let functions = kept
            .into_iter()
            .map(|f| {
                let pos = idx.as_ref().map(|idx| position_at(idx, f.start));
                let mut on_request_stack: Vec<String> = Vec::new();
                for &(request_id, offset) in &call_sites {
                    let inside = offset >= f.start && offset < f.end;
                    if inside && !on_request_stack.iter().any(|id| id == request_id) {
                        on_request_stack.push(request_id.to_owned());
                    }
                }
                let request_id = info
                    .and_then(|i| i.request_id.as_deref())
                    .filter(|id| !id.is_empty());
                TracedFunction {
                    name: if f.name.is_empty() {
                        "(anonymous)".into()
                    } else {
                        f.name.clone()
                    },
                    location: match pos {
                        Some(pos) => format!("{url}:{}:{}", pos.line, pos.column),
                        None => format!("{url}@{}", f.start),
                    },
                    calls: f.calls,
                    block_coverage: f.block_coverage,
                    on_request_stack,
                    resolve_source: pos.zip(request_id).map(|(pos, request_id)| ResolveSource {
                        request_id: request_id.to_owned(),
                        line: pos.line - 1,
                        column: pos.column.saturating_sub(1),
                    }),
                }
            })
            .collect();
        scripts.push(TracedScript { url, functions });
    }
    TraceSummary {
        total_functions: total,
        // `limit - budget` is how many functions were kept.
        omitted_functions: total - (input.limit - budget),
        scripts,
        requests: input
            .requests
            .iter()
            .map(|r| TracedRequest {
                request_id: r.request_id.clone(),
                method: r.method.clone(),
                url: r.url.clone(),
                status: r.status,
            })
            .collect(),
        limitations: &EXEC_TRACE_LIMITATIONS,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // offsets: "function a(){\n" = 0..13, "  b()\n" = 14..19, "}\n" = 20..21, "function b(){ return 1 }\n" = 22..46, "a()" = 47..49
    const SRC: &str = "function a(){\n  b()\n}\nfunction b(){ return 1 }\na()";

    fn coverage() -> Vec<ScriptCoverage> {
        serde_json::from_value(json!([
            {
                "scriptId": "9",
                "url": "https://x.io/app.js",
                "functions": [
                    { "functionName": "a", "isBlockCoverage": true, "ranges": [{ "startOffset": 0, "endOffset": 21, "count": 1 }] },
                    { "functionName": "b", "isBlockCoverage": true, "ranges": [{ "startOffset": 22, "endOffset": 46, "count": 3 }] }
                ]
            },
            {
                "scriptId": "10",
                "url": "https://x.io/idle.js",
                "functions": [{ "functionName": "z", "isBlockCoverage": true, "ranges": [{ "startOffset": 0, "endOffset": 5, "count": 0 }] }]
            }
        ]))
        .unwrap()
    }

    fn scripts() -> HashMap<String, ScriptInfo> {
        HashMap::from([(
            "9".to_owned(),
            ScriptInfo {
                url: "https://x.io/app.js".into(),
                source: Some(SRC.into()),
                request_id: Some("req-9".into()),
            },
        )])
    }

    fn requests() -> Vec<TraceRequest> {
        serde_json::from_value(json!([{
            "requestId": "r1",
            "method": "POST",
            "url": "https://x.io/api/sign",
            "status": 200,
            // call site inside b() → line 4 col 15 (0-based 3, 14)
            "initiatorStack": [{ "functionName": "b", "url": "https://x.io/app.js", "lineNumber": 3, "columnNumber": 14 }]
        }]))
        .unwrap()
    }

    fn summarize(limit: usize) -> TraceSummary {
        summarize_trace(TraceInput {
            coverage: &coverage(),
            scripts: &scripts(),
            requests: &requests(),
            limit,
        })
    }

    // positionAt / offsetAt

    #[test]
    fn utf16_offset을_1_based_line_col로_0_based_line_col을_offset으로_바꾼다() {
        let idx = line_index(SRC);
        assert_eq!(position_at(&idx, 0), Position { line: 1, column: 1 });
        assert_eq!(position_at(&idx, 22), Position { line: 4, column: 1 });
        assert_eq!(position_at(&idx, 16), Position { line: 2, column: 3 });
        assert_eq!(offset_at(&idx, 1, 2), Some(16));
        assert_eq!(offset_at(&idx, 99, 0), None);
    }

    // isTraceableScript

    #[test]
    fn 페이지_스크립트는_남긴다() {
        assert!(is_traceable_script("https://x.io/app.js", false));
        assert!(is_traceable_script("http://localhost:8779/", false));
    }

    #[test]
    fn electron_내부_확장_devtools_스크립트는_항상_뺀다() {
        assert!(!is_traceable_script(
            "node:electron/js2c/sandbox_bundle",
            true
        ));
        assert!(!is_traceable_script("chrome-extension://abc/x.js", true));
        assert!(!is_traceable_script("devtools://devtools/x.js", true));
    }

    #[test]
    fn url_없는_스크립트는_include_anonymous일_때만_남긴다() {
        assert!(!is_traceable_script("", false));
        assert!(is_traceable_script("", true));
    }

    // executedFunctions

    #[test]
    fn 구간_안에서_호출된_함수만_남기고_스크립트_최상위는_뺀다() {
        let cov: ScriptCoverage = serde_json::from_value(json!({
            "scriptId": "9",
            "url": "https://x.io/app.js",
            "functions": [
                { "functionName": "", "isBlockCoverage": true, "ranges": [{ "startOffset": 0, "endOffset": 50, "count": 1 }] },
                { "functionName": "a", "isBlockCoverage": true, "ranges": [{ "startOffset": 0, "endOffset": 21, "count": 2 }] },
                { "functionName": "b", "isBlockCoverage": false, "ranges": [{ "startOffset": 22, "endOffset": 46, "count": 0 }] }
            ]
        }))
        .unwrap();
        assert_eq!(
            executed_functions(&cov),
            [ExecutedFunction {
                name: "a".into(),
                start: 0,
                end: 21,
                calls: 2,
                block_coverage: true
            }]
        );
    }

    // summarizeTrace

    #[test]
    fn 실행된_함수를_스크립트별_위치와_호출_횟수_resolve_source_인자로_정리한다() {
        let s = summarize(50);
        assert_eq!(s.scripts.len(), 1);
        assert_eq!(s.scripts[0].url, "https://x.io/app.js");
        let rows: Vec<(&str, &str, i64)> = s.scripts[0]
            .functions
            .iter()
            .map(|f| (f.name.as_str(), f.location.as_str(), f.calls))
            .collect();
        assert_eq!(
            rows,
            [
                ("a", "https://x.io/app.js:1:1", 1),
                ("b", "https://x.io/app.js:4:1", 3)
            ]
        );
        assert_eq!(
            serde_json::to_value(&s.scripts[0].functions[1].resolve_source).unwrap(),
            json!({ "requestId": "req-9", "line": 3, "column": 0 })
        );
    }

    #[test]
    fn 요청의_initiator_스택이_지나간_함수를_on_request_stack으로_표시한다() {
        let s = summarize(50);
        let find = |name: &str| {
            s.scripts[0]
                .functions
                .iter()
                .find(|f| f.name == name)
                .unwrap()
        };
        assert_eq!(find("b").on_request_stack, ["r1"]);
        assert!(find("a").on_request_stack.is_empty());
        let request = serde_json::to_value(&s.requests[0]).unwrap();
        assert_eq!(request["requestId"], "r1");
        assert_eq!(request["method"], "POST");
        assert_eq!(request["status"], 200);
    }

    #[test]
    fn limit을_넘는_함수는_자르고_잘린_개수를_알려준다() {
        let s = summarize(1);
        assert_eq!(s.total_functions, 2);
        assert_eq!(s.omitted_functions, 1);
    }

    #[test]
    fn 원문이_없는_스크립트는_offset_위치로_표시한다() {
        let s = summarize_trace(TraceInput {
            coverage: &coverage(),
            scripts: &HashMap::new(),
            requests: &[],
            limit: 50,
        });
        assert_eq!(s.scripts[0].functions[0].location, "https://x.io/app.js@0");
    }

    // Not in the TypeScript suite: the JSON shape the tool hands to the agent.

    #[test]
    fn 요약은_타입스크립트와_같은_camel_case_필드로_직렬화된다() {
        let summary = serde_json::to_value(summarize(1)).unwrap();
        assert_eq!(summary["totalFunctions"], 2);
        assert_eq!(summary["omittedFunctions"], 1);
        assert_eq!(
            summary["scripts"],
            json!([{
                "url": "https://x.io/app.js",
                "functions": [{
                    "name": "a",
                    "location": "https://x.io/app.js:1:1",
                    "calls": 1,
                    "blockCoverage": true,
                    "onRequestStack": [],
                    "resolveSource": { "requestId": "req-9", "line": 0, "column": 0 }
                }]
            }])
        );
        assert_eq!(
            summary["requests"],
            json!([{ "requestId": "r1", "method": "POST", "url": "https://x.io/api/sign", "status": 200 }])
        );
        assert_eq!(summary["limitations"].as_array().unwrap().len(), 5);
    }
}
