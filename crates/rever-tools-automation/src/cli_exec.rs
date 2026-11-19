//! Port of `src/main/mcp/cli-exec.ts` — compiling captured requests into a site
//! CLI, the recording window of `cli_arm`, and running one saved command.
//!
//! A command without an extract is sent through a [`RequestSender`]. The
//! TypeScript sends it through the repeater, a fetch inside the logged-in tab;
//! the app plugs that in with `ctx.state::<SenderSlot>().set(…)`. Until it does,
//! [`ReqwestSender`] sends the request from this process, which has the saved
//! session headers but none of the tab's cookies.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rever_cli_spec::cli_spec::{
    CapturedCall, CliCommand, CliData, CliEffect, CliExtract, CompileOptions, ExtractSpec,
    OUTCOME_UNKNOWN, Record, build_cli_request, build_extract_expression, compile_cli,
    parse_extract, present_raw_body, run_gate, scrub_extract_items, scrub_secrets, static_asset,
};
use rever_core::traffic::ListFilter;
use rever_scope::fence::{fence_reason, is_fence_active, record_fence_block};
use rever_scope::scope::{get_scope, scope_block_for_url};
use rever_tools_api::tool::{BoxFuture, ToolContext, unsupported};
use serde::Serialize;
use serde_json::{Number, Value};
use url::Url;

use crate::js::{js_truthy, now_ms};
use crate::lock;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliRunResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// True when an HTML body was shortened so the page is not the result.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub body_cut: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Record<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl CliRunResult {
    fn failed(error: impl Into<String>) -> Self {
        Self {
            error: Some(error.into()),
            ..Self::default()
        }
    }
}

/// `Err` is the message of the error `compileCli` throws.
pub fn compile_captured(
    ctx: &ToolContext,
    ids: &[String],
    opts: &CompileOptions,
) -> Result<CliData, String> {
    let traffic = lock(&ctx.traffic);
    let mut calls: Vec<CapturedCall> = Vec::new();
    for id in ids {
        let Some(row) = traffic.get(id) else { continue };
        let headers = row.request_headers.as_ref();
        calls.push(CapturedCall {
            request_id: row.request_id.clone(),
            method: row.method.clone(),
            url: row.url.clone(),
            resource_type: Some(row.resource_type.clone()),
            status: row.status,
            mime_type: row.mime_type.clone(),
            request_headers: headers.map(|headers| headers.clone().into_iter().collect()),
            request_post_data: row.request_post_data.clone(),
            response_body: match row.response_body_base64 {
                Some(true) => None,
                _ => row.response_body.clone(),
            },
        });
    }
    drop(traffic);
    compile_cli(&calls, opts)
}

#[derive(Clone, Copy)]
struct ArmWindow {
    started_at: u64,
    stopped_at: Option<u64>,
}

/// The recording window of `cli_arm`.
#[derive(Default)]
pub struct CliArm(Mutex<Option<ArmWindow>>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmAction {
    Start,
    Stop,
    List,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmedCall {
    pub request_id: String,
    pub method: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmState {
    pub recording: bool,
    pub started_at: Option<u64>,
    pub stopped_at: Option<u64>,
    pub requests: Vec<ArmedCall>,
}

pub fn arm_cli(ctx: &ToolContext, action: ArmAction) -> ArmState {
    let window = {
        let arm = ctx.state::<CliArm>();
        let mut armed = lock(&arm.0);
        match (action, armed.as_mut()) {
            (ArmAction::Start, _) => {
                *armed = Some(ArmWindow {
                    started_at: now_ms(),
                    stopped_at: None,
                });
            }
            (ArmAction::Stop, Some(window)) if window.stopped_at.is_none() => {
                window.stopped_at = Some(now_ms());
            }
            _ => {}
        }
        *armed
    };
    ArmState {
        recording: window.is_some_and(|window| window.stopped_at.is_none()),
        started_at: window.map(|window| window.started_at),
        stopped_at: window.and_then(|window| window.stopped_at),
        requests: window.map_or_else(Vec::new, |window| calls_in_window(ctx, window)),
    }
}

fn calls_in_window(ctx: &ToolContext, window: ArmWindow) -> Vec<ArmedCall> {
    let end = window.stopped_at.unwrap_or(u64::MAX);
    let traffic = lock(&ctx.traffic);
    let rows = traffic.list(&ListFilter {
        since: Some(window.started_at),
        limit: Some(200),
        ..ListFilter::default()
    });
    let mut out = Vec::new();
    for row in rows {
        if row.started_at > end {
            continue;
        }
        let kind = row.resource_type.to_lowercase();
        if kind != "xhr" && kind != "fetch" {
            continue;
        }
        match Url::parse(&row.url) {
            Ok(url) if !static_asset(url.path()) => {}
            _ => continue,
        }
        out.push(ArmedCall {
            request_id: row.request_id.clone(),
            method: row.method.clone(),
            url: row.url.clone(),
        });
    }
    out
}

/// `URL.origin` as text: an opaque origin is "null", so two of them compare equal.
fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Header values copied into the request only. Never returned to the model.
pub fn session_header_values(ctx: &ToolContext, origin: &str, names: &[String]) -> Record<String> {
    let mut found: Record<String> = Record::new();
    if names.is_empty() {
        return found;
    }
    let Ok(base) = Url::parse(origin) else {
        return found;
    };
    let mut want: Vec<String> = names.iter().map(|name| name.to_lowercase()).collect();
    want.sort();
    want.dedup();
    // `URL.host`: the port is part of it unless it is the scheme's default.
    let host = match (base.host_str(), base.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => String::new(),
    };
    let traffic = lock(&ctx.traffic);
    let rows = traffic.list(&ListFilter {
        host: Some(host),
        limit: Some(200),
        ..ListFilter::default()
    });
    for row in rows {
        match Url::parse(&row.url) {
            Ok(url) if origin_of(&url) == origin_of(&base) => {}
            _ => continue,
        }
        for (key, value) in row.request_headers.iter().flatten() {
            let lower = key.to_lowercase();
            if !want.contains(&lower) || value.is_empty() || found.contains_key(&lower) {
                continue;
            }
            found.insert(lower, value.clone());
        }
        if found.len() == want.len() {
            break;
        }
    }
    found
}

// ── Sending one request ─────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendRequest {
    pub url: String,
    pub method: String,
    pub headers: Record<String>,
    pub body: Option<String>,
}

/// The part of the repeater's response a CLI run reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SendResponse {
    /// 0 when no response arrived.
    pub status: u16,
    pub headers: Record<String>,
    pub body: String,
    /// The body was cut at the sender's size cap.
    pub body_truncated: bool,
    /// The request was sent, or may have been, and failed: a network error or a
    /// timeout. A run of a write command then reports `action_outcome_unknown`.
    pub error: Option<String>,
}

/// Sends one request the way the repeater does. `Err` is a refusal before
/// anything was sent (out of scope, no page to send from).
pub trait RequestSender: Send + Sync {
    fn send<'a>(
        &'a self,
        ctx: &'a ToolContext,
        request: &'a SendRequest,
    ) -> BoxFuture<'a, Result<SendResponse, String>>;
}

/// The sender CLI runs use.
pub struct SenderSlot(Mutex<Arc<dyn RequestSender>>);

impl Default for SenderSlot {
    fn default() -> Self {
        Self(Mutex::new(Arc::new(ReqwestSender)))
    }
}

impl SenderSlot {
    pub fn set(&self, sender: Arc<dyn RequestSender>) {
        *lock(&self.0) = sender;
    }

    pub fn get(&self) -> Arc<dyn RequestSender> {
        lock(&self.0).clone()
    }
}

/// The repeater's response-body cap and fetch timeout.
const MAX_BODY_BYTES: usize = 64 * 1024;
const FETCH_TIMEOUT_MS: u64 = 15_000;

/// Sends from this process over reqwest.
pub struct ReqwestSender;

impl RequestSender for ReqwestSender {
    fn send<'a>(
        &'a self,
        _ctx: &'a ToolContext,
        request: &'a SendRequest,
    ) -> BoxFuture<'a, Result<SendResponse, String>> {
        Box::pin(async move {
            // Scope guardrail, as in the repeater: refuse targets outside the
            // session scope (no-op until a scope is set).
            if let Some(blocked) = scope_block_for_url(Some(&request.url)) {
                return Err(blocked);
            }
            // The capture-not-send fence sits between the page and the network, so
            // a request sent from here would walk around it. Answer what the page
            // would have been answered: a synthetic 204, recorded.
            if is_fence_active()
                && get_scope().enabled
                && let Some(reason) = fence_reason(&request.method, &request.url)
            {
                record_fence_block(&request.method, &request.url, reason);
                return Ok(SendResponse {
                    status: 204,
                    ..SendResponse::default()
                });
            }
            let limit = Duration::from_millis(FETCH_TIMEOUT_MS);
            let sent = tokio::time::timeout(limit, fetch(request)).await;
            Ok(match sent {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => SendResponse {
                    error: Some(error),
                    ..SendResponse::default()
                },
                Err(_) => SendResponse {
                    error: Some(format!(
                        "repeater: fetch aborted after {FETCH_TIMEOUT_MS}ms with no response (target likely hung)"
                    )),
                    ..SendResponse::default()
                },
            })
        })
    }
}

async fn fetch(request: &SendRequest) -> Result<SendResponse, String> {
    // Not `Client::new()`, which panics when the TLS backend cannot start.
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    let message = |e: reqwest::Error| e.to_string();
    let client = CLIENT
        .get_or_init(|| reqwest::Client::builder().build().map_err(message))
        .as_ref()
        .map_err(String::clone)?;
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|_| format!("'{}' is not a valid HTTP method.", request.method))?;
    let has_body = method != reqwest::Method::GET && method != reqwest::Method::HEAD;
    let mut builder = client.request(method, &request.url);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    if let Some(body) = request.body.as_ref().filter(|_| has_body) {
        builder = builder.body(body.clone());
    }
    let mut response = builder.send().await.map_err(message)?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
            (name.as_str().to_owned(), value)
        })
        .collect();
    // Stop reading once past the cap: the rest would be thrown away.
    let mut bytes: Vec<u8> = Vec::new();
    while bytes.len() <= MAX_BODY_BYTES {
        match response.chunk().await.map_err(message)? {
            Some(chunk) => bytes.extend_from_slice(&chunk),
            None => break,
        }
    }
    let body_truncated = bytes.len() > MAX_BODY_BYTES;
    bytes.truncate(MAX_BODY_BYTES);
    // shortcut: the body is read as UTF-8 whatever charset the response declares
    // (the repeater decodes euc-kr and the like). The repeater replaces this
    // sender; add a decoder here if it ever stays.
    let text = String::from_utf8_lossy(&bytes);
    let mut body = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if body_truncated {
        // Cutting at a byte cap can split a multi-byte character; drop the
        // replacement characters it decodes to.
        body = body.trim_end_matches('\u{fffd}');
    }
    Ok(SendResponse {
        status,
        headers,
        body: body.to_owned(),
        body_truncated,
        error: None,
    })
}

// ── Running one command ─────────────────────────────────────────────────────

pub struct CliRun<'a> {
    pub base_url: &'a str,
    pub writes: bool,
    pub command: &'a CliCommand,
    pub inputs: &'a Record<String>,
}

pub async fn execute_cli(ctx: &ToolContext, input: CliRun<'_>) -> CliRunResult {
    let command = input.command;
    if let Some(blocked) = run_gate(command.effect, input.writes) {
        return CliRunResult::failed(blocked);
    }
    let built = match build_cli_request(input.base_url, command, input.inputs) {
        Ok(built) => built,
        Err(error) => return CliRunResult::failed(error),
    };
    let body = built.body.filter(|body| !body.is_empty());

    let mut headers: Record<String> = Record::new();
    let content_type = match &command.body {
        _ if body.is_none() => None,
        Some(shape) if shape.json.is_some() => Some("application/json"),
        Some(shape) if shape.form.is_some() => Some("application/x-www-form-urlencoded"),
        _ => None,
    };
    if let Some(content_type) = content_type {
        headers.insert("content-type".to_owned(), content_type.to_owned());
    }
    headers.extend(session_header_values(
        ctx,
        input.base_url,
        &command.session_headers,
    ));

    // The saved extract goes through the same checks as one an agent passes.
    let saved = serde_json::to_value(&command.extract).unwrap_or(Value::Null);
    let extract = match parse_extract(&saved) {
        Ok(extract) => extract,
        Err(error) => return CliRunResult::failed(error),
    };
    if let Some(extract) = extract {
        let spec = ExtractSpec {
            url: built.url,
            method: built.method,
            headers,
            body,
            extract,
        };
        return execute_extract(ctx, command.effect, &spec).await;
    }

    let request = SendRequest {
        url: built.url,
        method: built.method,
        headers,
        body,
    };
    let sender = ctx.state::<SenderSlot>().get();
    let response = match sender.send(ctx, &request).await {
        Ok(response) => response,
        Err(error) => return CliRunResult::failed(error),
    };

    let error = response.error.filter(|error| !error.is_empty());
    if error.is_some() && command.effect != CliEffect::Read {
        return CliRunResult::failed(OUTCOME_UNKNOWN);
    }
    let presented = present_raw_body(&scrub_secrets(&response.body), response.body_truncated);
    CliRunResult {
        ok: error.is_none(),
        error: error.as_deref().map(scrub_secrets),
        status: Some(response.status.into()),
        body: Some(presented.body),
        body_cut: presented.body_cut,
        ..CliRunResult::default()
    }
}

fn as_items(value: Option<&Value>) -> Vec<Record<String>> {
    let rows = value.and_then(Value::as_array);
    let rows = rows.into_iter().flatten().filter_map(Value::as_object);
    rows.map(|row| {
        let fields = row
            .iter()
            .filter_map(|(key, field)| Some((key.clone(), field.as_str()?.to_owned())));
        fields.collect()
    })
    .collect()
}

/// Read the full response in the tab and return rows. The page itself stays in the tab.
async fn execute_extract(ctx: &ToolContext, effect: CliEffect, spec: &ExtractSpec) -> CliRunResult {
    if let Some(blocked) = scope_block_for_url(Some(&spec.url)) {
        return CliRunResult::failed(blocked);
    }
    let unknown_unless_read = |error: &str| {
        if effect == CliEffect::Read {
            CliRunResult::failed(scrub_secrets(error))
        } else {
            CliRunResult::failed(OUTCOME_UNKNOWN)
        }
    };

    let page = match ctx.browser.evaluate(&build_extract_expression(spec)).await {
        Ok(page) => page,
        // The engine cannot evaluate at all: nothing was sent, so say so plainly.
        Err(error) if error == unsupported("evaluate") => return CliRunResult::failed(error),
        // Otherwise the script may have failed after the request left, the case
        // the TypeScript reads off `exceptionDetails`.
        Err(error) => return unknown_unless_read(&error),
    };
    if !js_truthy(&page) {
        return CliRunResult::failed("empty result");
    }
    let status = page.get("status").and_then(Value::as_number).cloned();
    if let Some(error) = page
        .get("error")
        .and_then(Value::as_str)
        .filter(|e| !e.is_empty())
    {
        return CliRunResult {
            // An unknown outcome carries nothing but its name.
            status: status.filter(|_| effect == CliEffect::Read),
            ..unknown_unless_read(error)
        };
    }
    let extract: &CliExtract = &spec.extract;
    let mut items = scrub_extract_items(&as_items(page.get("items")), &extract.fields);
    items.truncate(extract.limit as usize);
    let matched = page.get("matched").and_then(Value::as_number).cloned();
    let note = page
        .get("note")
        .and_then(Value::as_str)
        .filter(|note| !note.is_empty());
    CliRunResult {
        ok: true,
        status,
        matched: Some(matched.unwrap_or_else(|| items.len().into())),
        items: Some(items),
        note: note.map(scrub_secrets),
        ..CliRunResult::default()
    }
}

#[cfg(test)]
mod tests {
    use rever_cli_spec::cli_spec::{CliExtractField, WRITES_DISABLED};
    use rever_scope::fence::{acquire_fence, get_fence_blocks, release_fence};
    use rever_scope::scope::{clear_scope, set_scope};
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::test_support::{
        FakeBrowser, FakeSender, capture, capture_shop, context, context_on, headers,
    };

    fn names(pairs: &[(&str, &str)]) -> CompileOptions {
        let names = pairs
            .iter()
            .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()));
        CompileOptions {
            names: Some(names.collect()),
            ..CompileOptions::default()
        }
    }

    /// The shop's four calls compiled: `search`, `create`, `item` and `page`.
    fn shop(ctx: &ToolContext) -> CliData {
        capture_shop(ctx);
        let ids: Vec<String> = ["r1", "r2", "r3", "r4"].map(str::to_owned).to_vec();
        let opts = names(&[
            ("r1", "search"),
            ("r2", "create"),
            ("r3", "item"),
            ("r4", "page"),
        ]);
        compile_captured(ctx, &ids, &opts).unwrap()
    }

    fn command<'a>(data: &'a CliData, name: &str) -> &'a CliCommand {
        data.commands
            .iter()
            .find(|command| command.name == name)
            .unwrap()
    }

    fn inputs(pairs: &[(&str, &str)]) -> Record<String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    async fn run(
        ctx: &ToolContext,
        data: &CliData,
        name: &str,
        writes: bool,
        pairs: &[(&str, &str)],
    ) -> CliRunResult {
        let run = CliRun {
            base_url: &data.base_url,
            writes,
            command: command(data, name),
            inputs: &inputs(pairs),
        };
        execute_cli(ctx, run).await
    }

    #[test]
    fn 캡처한_요청을_명령으로_컴파일한다() {
        let ctx = context();
        let data = shop(&ctx);
        assert_eq!(data.base_url, "https://shop.test");
        assert!(!data.writes);
        let search = command(&data, "search");
        assert_eq!(
            (search.method.as_str(), search.path.as_str()),
            ("GET", "/api/items")
        );
        assert_eq!(search.effect, CliEffect::Read);
        assert_eq!(search.evidence_request_ids, ["r1"]);
        assert_eq!(search.session_headers, ["x-csrf-token"]);
        assert_eq!(search.response_pointers, ["/ok"]);
        assert!(search.query.contains_key("q"));
        assert_eq!(command(&data, "create").effect, CliEffect::Write);
        assert_eq!(command(&data, "item").path, "/api/items/{id0}");
    }

    #[test]
    fn 모르는_id는_건너뛰고_base64_본문은_컴파일에_넘기지_않는다() {
        let ctx = context();
        capture(&ctx, "bin", "GET", "https://shop.test/api/blob", |patch| {
            patch.response_body = Some("eyJvayI6dHJ1ZX0=".to_owned());
            patch.response_body_base64 = Some(true);
        });
        let ids = ["gone".to_owned(), "bin".to_owned()];
        let data = compile_captured(&ctx, &ids, &CompileOptions::default()).unwrap();
        assert_eq!(data.commands.len(), 1);
        assert_eq!(data.commands[0].evidence_request_ids, ["bin"]);
        assert!(data.commands[0].response_pointers.is_empty());

        let none = compile_captured(&ctx, &["gone".to_owned()], &CompileOptions::default());
        assert!(none.unwrap().commands.is_empty());
        // What compileCli throws comes back as the error text.
        let opts = CompileOptions {
            base_url: Some("not a url".to_owned()),
            ..CompileOptions::default()
        };
        assert_eq!(
            compile_captured(&ctx, &ids, &opts),
            Err("Invalid URL".to_owned())
        );
    }

    #[test]
    fn 녹화_창은_시작과_정지_사이의_xhr과_fetch만_돌려준다() {
        let ctx = context();
        assert_eq!(
            serde_json::to_value(arm_cli(&ctx, ArmAction::List)).unwrap(),
            json!({ "recording": false, "startedAt": null, "stoppedAt": null, "requests": [] })
        );
        // Stop without a window changes nothing.
        assert_eq!(arm_cli(&ctx, ArmAction::Stop).started_at, None);

        let started = arm_cli(&ctx, ArmAction::Start);
        assert!(started.recording);
        let t0 = started.started_at.unwrap();
        // Everything inside the window starts in its first millisecond, so the
        // stop below, however soon it comes, is not before any of it.
        capture(
            &ctx,
            "before",
            "GET",
            "https://shop.test/api/old",
            |patch| {
                patch.started_at = Some(t0 - 1);
            },
        );
        capture(&ctx, "a", "GET", "https://shop.test/api/a", |patch| {
            patch.started_at = Some(t0);
        });
        capture(&ctx, "b", "POST", "https://shop.test/api/b", |patch| {
            patch.started_at = Some(t0);
            patch.resource_type = Some("XHR".to_owned());
        });
        capture(&ctx, "doc", "GET", "https://shop.test/", |patch| {
            patch.started_at = Some(t0);
            patch.resource_type = Some("Document".to_owned());
        });
        capture(
            &ctx,
            "asset",
            "GET",
            "https://shop.test/static/App.JS?v=1",
            |patch| {
                patch.started_at = Some(t0);
            },
        );
        capture(&ctx, "broken", "GET", "not a url", |patch| {
            patch.started_at = Some(t0);
        });

        let listed = arm_cli(&ctx, ArmAction::List);
        assert_eq!(
            serde_json::to_value(&listed.requests).unwrap(),
            json!([
                { "requestId": "b", "method": "POST", "url": "https://shop.test/api/b" },
                { "requestId": "a", "method": "GET", "url": "https://shop.test/api/a" },
            ])
        );

        let stopped = arm_cli(&ctx, ArmAction::Stop);
        assert!(!stopped.recording);
        let t1 = stopped.stopped_at.unwrap();
        assert!(t1 >= t0);
        capture(&ctx, "late", "GET", "https://shop.test/api/late", |patch| {
            patch.started_at = Some(t1 + 1);
        });
        // A second stop keeps the first stop time, and the list stays closed.
        let again = arm_cli(&ctx, ArmAction::Stop);
        assert_eq!(again.stopped_at, Some(t1));
        assert_eq!(again.requests.len(), 2);

        let restarted = arm_cli(&ctx, ArmAction::Start);
        assert!(restarted.recording);
        assert_eq!(restarted.stopped_at, None);
    }

    #[test]
    fn 세션_헤더_값은_같은_출처의_최근_요청에서_찾는다() {
        let ctx = context();
        let want = ["X-CSRF-Token".to_owned(), "authorization".to_owned()];
        capture(&ctx, "old", "GET", "https://shop.test/a", |patch| {
            patch.request_headers =
                headers(&[("x-csrf-token", "old"), ("Authorization", "Bearer old")]);
        });
        capture(&ctx, "other-origin", "GET", "http://shop.test/a", |patch| {
            patch.request_headers = headers(&[("x-csrf-token", "http")]);
        });
        capture(
            &ctx,
            "other-port",
            "GET",
            "https://shop.test:8443/a",
            |patch| {
                patch.request_headers = headers(&[("x-csrf-token", "port")]);
            },
        );
        capture(&ctx, "new", "GET", "https://shop.test/b", |patch| {
            patch.request_headers = headers(&[("X-Csrf-Token", "new"), ("authorization", "")]);
        });
        // Newest first; an empty value does not count as found.
        let found = session_header_values(&ctx, "https://shop.test", &want);
        assert_eq!(
            found,
            inputs(&[("x-csrf-token", "new"), ("authorization", "Bearer old")])
        );

        let on_port = session_header_values(&ctx, "https://shop.test:8443/", &want);
        assert_eq!(on_port, inputs(&[("x-csrf-token", "port")]));
        assert!(session_header_values(&ctx, "https://shop.test", &[]).is_empty());
        assert!(session_header_values(&ctx, "not a url", &want).is_empty());
        assert!(session_header_values(&ctx, "https://nobody.test", &want).is_empty());
    }

    #[tokio::test]
    async fn 읽기_명령은_세션_헤더를_붙여_보내고_본문을_돌려준다() {
        let ctx = context();
        let data = shop(&ctx);
        let sender = FakeSender::body(&ctx, 200, r#"{"items":[1]}"#);
        let result = run(&ctx, &data, "search", false, &[("q", "red shoes")]).await;
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({ "ok": true, "status": 200, "body": "{\"items\":[1]}" })
        );
        let sent = lock(&sender.sent);
        assert_eq!(
            *sent,
            [SendRequest {
                url: "https://shop.test/api/items?q=red+shoes".to_owned(),
                method: "GET".to_owned(),
                // The newest capture of that origin carries the header.
                headers: inputs(&[("x-csrf-token", "tok2")]),
                body: None,
            }]
        );
    }

    #[tokio::test]
    async fn 쓰기_명령은_쓰기가_꺼져_있으면_보내지_않는다() {
        let ctx = context();
        let data = shop(&ctx);
        let sender = FakeSender::body(&ctx, 200, "{}");
        let result = run(&ctx, &data, "create", false, &[("title", "Boot")]).await;
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({ "ok": false, "error": WRITES_DISABLED })
        );
        assert!(lock(&sender.sent).is_empty());

        let result = run(&ctx, &data, "create", true, &[("title", "Boot")]).await;
        assert!(result.ok);
        let sent = lock(&sender.sent);
        assert_eq!(sent[0].method, "POST");
        assert_eq!(sent[0].body.as_deref(), Some(r#"{"title":"Boot"}"#));
        assert_eq!(
            sent[0].headers,
            inputs(&[
                ("content-type", "application/json"),
                ("x-csrf-token", "tok2")
            ])
        );
    }

    #[tokio::test]
    async fn 폼_본문은_폼_콘텐츠_타입으로_보내고_본문이_없으면_타입도_없다() {
        let ctx = context();
        capture(&ctx, "f1", "POST", "https://shop.test/login", |patch| {
            patch.request_headers =
                headers(&[("Content-Type", "application/x-www-form-urlencoded")]);
            patch.request_post_data = Some("user=kim&note=hi".to_owned());
        });
        let data = compile_captured(&ctx, &["f1".to_owned()], &names(&[("f1", "login")])).unwrap();
        let login = command(&data, "login");
        assert!(login.body.as_ref().is_some_and(|body| body.form.is_some()));
        let sender = FakeSender::body(&ctx, 200, "ok");
        let result = run(
            &ctx,
            &data,
            "login",
            true,
            &[("user", "lee kim"), ("note", "a&b")],
        )
        .await;
        assert!(result.ok);
        {
            let sent = lock(&sender.sent);
            assert_eq!(sent[0].body.as_deref(), Some("user=lee+kim&note=a%26b"));
            assert_eq!(
                sent[0].headers,
                inputs(&[("content-type", "application/x-www-form-urlencoded")])
            );
        }

        // A command that sends no body says nothing about its type either.
        let mut bare = login.clone();
        for param in bare.body.iter_mut().flat_map(|body| body.form.iter_mut()) {
            for (_, param) in param.iter_mut() {
                param.required = false;
            }
        }
        let run = CliRun {
            base_url: &data.base_url,
            writes: true,
            command: &bare,
            inputs: &Record::new(),
        };
        assert!(execute_cli(&ctx, run).await.ok);
        let sent = lock(&sender.sent);
        assert_eq!(sent[1].body, None);
        assert!(sent[1].headers.is_empty());
    }

    #[test]
    fn 녹화_창은_최근_200개까지만_본다() {
        let ctx = context();
        let t0 = arm_cli(&ctx, ArmAction::Start).started_at.unwrap();
        for n in 0..205 {
            let url = format!("https://shop.test/api/n{n}");
            capture(&ctx, &format!("n{n}"), "GET", &url, |patch| {
                patch.started_at = Some(t0);
            });
        }
        let listed = arm_cli(&ctx, ArmAction::List).requests;
        assert_eq!(listed.len(), 200);
        // Newest first: the five oldest are the ones left out.
        assert_eq!(listed[0].request_id, "n204");
        assert_eq!(listed[199].request_id, "n5");
    }

    #[tokio::test]
    async fn 필요한_값이_없으면_보내기_전에_실패한다() {
        let ctx = context();
        let data = shop(&ctx);
        let sender = FakeSender::body(&ctx, 200, "{}");
        let result = run(&ctx, &data, "item", false, &[]).await;
        assert_eq!(result.error.as_deref(), Some("missing id0"));
        assert!(lock(&sender.sent).is_empty());

        let result = run(&ctx, &data, "item", false, &[("id0", "a/b")]).await;
        assert!(result.ok);
        assert_eq!(
            lock(&sender.sent)[0].url,
            "https://shop.test/api/items/a%2Fb"
        );
    }

    #[tokio::test]
    async fn 보낸_뒤의_실패는_쓰기면_결과를_알_수_없다고_답한다() {
        let ctx = context();
        let data = shop(&ctx);
        let failed = SendResponse {
            error: Some("Failed to fetch with Bearer abc.def-123".to_owned()),
            ..SendResponse::default()
        };
        FakeSender::answering(&ctx, Ok(failed));
        let write = run(&ctx, &data, "create", true, &[("title", "Boot")]).await;
        assert_eq!(
            serde_json::to_value(&write).unwrap(),
            json!({ "ok": false, "error": OUTCOME_UNKNOWN })
        );
        // A read can simply be told what went wrong, with secrets scrubbed.
        let read = run(&ctx, &data, "search", false, &[]).await;
        assert_eq!(
            serde_json::to_value(&read).unwrap(),
            json!({ "ok": false, "error": "Failed to fetch with Bearer ****", "status": 0, "body": "" })
        );

        // A refusal before anything was sent is reported as it is, even for a write.
        FakeSender::answering(&ctx, Err("Out of scope: x".to_owned()));
        let refused = run(&ctx, &data, "create", true, &[("title", "Boot")]).await;
        assert_eq!(refused.error.as_deref(), Some("Out of scope: x"));
        assert_eq!(refused.status, None);
    }

    #[tokio::test]
    async fn html_페이지는_미리보기로_줄이고_비밀값은_가린다() {
        let ctx = context();
        let data = shop(&ctx);
        let page = format!("<!doctype html><html>{}</html>", "가".repeat(3_000));
        FakeSender::body(&ctx, 200, &page);
        let result = run(&ctx, &data, "search", false, &[]).await;
        assert!(result.ok && result.body_cut);
        assert_eq!(
            result.body.as_deref().map(|b| b.chars().count()),
            Some(1_500)
        );

        FakeSender::body(&ctx, 200, r#"{"token":"Bearer abc.def-123"}"#);
        let result = run(&ctx, &data, "search", false, &[]).await;
        assert_eq!(result.body.as_deref(), Some(r#"{"token":"Bearer ****"}"#));
        assert!(!result.body_cut);
        assert!(
            serde_json::to_value(&result)
                .unwrap()
                .get("bodyCut")
                .is_none()
        );
    }

    fn with_extract(data: &CliData, name: &str, effect: CliEffect) -> CliCommand {
        let mut page = command(data, name).clone();
        page.effect = effect;
        page.extract = Some(CliExtract {
            selector: "li.item".to_owned(),
            limit: 2,
            fields: vec![
                CliExtractField {
                    name: "title".to_owned(),
                    selector: Some("a".to_owned()),
                    attr: None,
                },
                CliExtractField {
                    name: "link".to_owned(),
                    selector: Some("a".to_owned()),
                    attr: Some("href".to_owned()),
                },
            ],
        });
        page
    }

    async fn run_extract(
        answer: Result<Value, String>,
        effect: CliEffect,
    ) -> (CliRunResult, Vec<String>, usize) {
        let browser = FakeBrowser::new(answer);
        let ctx = context_on(browser.clone());
        let data = shop(&ctx);
        let sender = FakeSender::body(&ctx, 200, "never");
        let page = with_extract(&data, "page", effect);
        let run = CliRun {
            base_url: &data.base_url,
            writes: true,
            command: &page,
            inputs: &Record::new(),
        };
        let result = execute_cli(&ctx, run).await;
        let evaluated = lock(&browser.evaluated).clone();
        let sent = lock(&sender.sent).len();
        (result, evaluated, sent)
    }

    #[tokio::test]
    async fn 추출이_있는_명령은_탭_안에서_읽어_필드만_돌려준다() {
        let page = json!({
            "status": 200,
            "matched": 12,
            "items": [
                { "link": "/a", "title": "A Bearer abc.def-123", "n": 5 },
                "junk",
                { "title": "B" },
                { "title": "C" }
            ],
            "note": "page read was capped"
        });
        let (result, evaluated, sent) = run_extract(Ok(page), CliEffect::Read).await;
        // Rows in field order, cut at the limit, only string fields, scrubbed.
        assert_eq!(
            serde_json::to_string(&result).unwrap(),
            r#"{"ok":true,"status":200,"items":[{"title":"A Bearer ****","link":"/a"},{"title":"B"}],"matched":12,"note":"page read was capped"}"#
        );
        // The page did the fetch; nothing went through the sender.
        assert_eq!(sent, 0);
        assert_eq!(evaluated.len(), 1);
        assert!(evaluated[0].contains(r#""url":"https://shop.test/list""#));
        assert!(evaluated[0].contains(r#""selector":"li.item""#));
    }

    #[tokio::test]
    async fn 추출_결과에_개수와_상태가_없으면_행_수를_센다() {
        let (result, ..) =
            run_extract(Ok(json!({ "items": [{ "title": "A" }] })), CliEffect::Read).await;
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({ "ok": true, "items": [{ "title": "A" }], "matched": 1 })
        );
        let (result, ..) = run_extract(
            Ok(json!({ "items": "nope", "matched": "3" })),
            CliEffect::Read,
        )
        .await;
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({ "ok": true, "items": [], "matched": 0 })
        );
        let (result, ..) = run_extract(Ok(Value::Null), CliEffect::Read).await;
        assert_eq!(result.error.as_deref(), Some("empty result"));
    }

    #[tokio::test]
    async fn 추출_중_실패는_읽기면_이유를_쓰기면_알_수_없음을_답한다() {
        let failed = json!({ "error": "HTTP 500 Bearer abc.def-123", "status": 500 });
        let (read, ..) = run_extract(Ok(failed.clone()), CliEffect::Read).await;
        assert_eq!(
            serde_json::to_value(&read).unwrap(),
            json!({ "ok": false, "error": "HTTP 500 Bearer ****", "status": 500 })
        );
        let (write, ..) = run_extract(Ok(failed), CliEffect::Write).await;
        assert_eq!(
            serde_json::to_value(&write).unwrap(),
            json!({ "ok": false, "error": OUTCOME_UNKNOWN })
        );

        // The script throwing, which may be after the request left.
        let thrown = || Err("TypeError: Bearer abc.def-123".to_owned());
        let (read, ..) = run_extract(thrown(), CliEffect::Read).await;
        assert_eq!(read.error.as_deref(), Some("TypeError: Bearer ****"));
        let (unknown, ..) = run_extract(thrown(), CliEffect::Unknown).await;
        assert_eq!(unknown.error.as_deref(), Some(OUTCOME_UNKNOWN));
    }

    #[tokio::test]
    async fn 평가를_못_하는_엔진에서는_추출이_보내지_않았다고_답한다() {
        let ctx = context();
        let data = shop(&ctx);
        let page = with_extract(&data, "page", CliEffect::Write);
        let run = CliRun {
            base_url: &data.base_url,
            writes: true,
            command: &page,
            inputs: &Record::new(),
        };
        let result = execute_cli(&ctx, run).await;
        assert_eq!(result.error, Some(unsupported("evaluate")));
    }

    /// Answers one HTTP request with `response` and returns what it received.
    async fn serve_once(response: Vec<u8>) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/echo?x=1", listener.local_addr().unwrap());
        let served = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut buffer = [0_u8; 4096];
            // The requests here are small: the head and the body arrive together,
            // and the body ends with a marker.
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                received.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&received);
                if read == 0
                    || (text.contains("\r\n\r\n")
                        && (text.starts_with("GET") || text.ends_with("<end>")))
                {
                    break;
                }
            }
            socket.write_all(&response).await.unwrap();
            socket.shutdown().await.unwrap();
            String::from_utf8_lossy(&received).into_owned()
        });
        (url, served)
    }

    fn http(status: &str, extra_headers: &str, body: &[u8]) -> Vec<u8> {
        let head = format!(
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n{extra_headers}\r\n",
            body.len()
        );
        [head.as_bytes(), body].concat()
    }

    #[tokio::test]
    async fn 기본_전송기는_요청을_보내고_응답을_읽는다() {
        let ctx = context();
        let body = "\u{feff}{\"saved\":true}".as_bytes();
        let (url, served) = serve_once(http("201 Created", "x-reply: yes\r\n", body)).await;
        let request = SendRequest {
            url,
            method: "POST".to_owned(),
            headers: inputs(&[
                ("content-type", "application/json"),
                ("x-csrf-token", "tok"),
            ]),
            body: Some("{\"title\":\"Boot\"}<end>".to_owned()),
        };
        let response = ReqwestSender.send(&ctx, &request).await.unwrap();
        assert_eq!(response.status, 201);
        assert_eq!(
            response.headers.get("x-reply").map(String::as_str),
            Some("yes")
        );
        // The byte-order mark is not part of the text.
        assert_eq!(response.body, "{\"saved\":true}");
        assert!(!response.body_truncated);
        assert_eq!(response.error, None);

        let received = served.await.unwrap().to_lowercase();
        assert!(received.starts_with("post /echo?x=1 http/1.1\r\n"));
        assert!(received.contains("\r\nx-csrf-token: tok\r\n"));
        assert!(received.contains("\r\ncontent-type: application/json\r\n"));
        assert!(received.ends_with("\r\n\r\n{\"title\":\"boot\"}<end>"));
    }

    #[tokio::test]
    async fn 기본_전송기는_큰_본문을_상한에서_자르고_get에는_본문을_싣지_않는다() {
        let ctx = context();
        // 3-byte characters: the 64 KiB cap falls inside one.
        let big = "가".repeat(30_000);
        let (url, served) = serve_once(http("200 OK", "", big.as_bytes())).await;
        let request = SendRequest {
            url,
            method: "GET".to_owned(),
            headers: Record::new(),
            body: Some("ignored".to_owned()),
        };
        let response = ReqwestSender.send(&ctx, &request).await.unwrap();
        assert!(response.body_truncated);
        assert_eq!(response.body, "가".repeat(MAX_BODY_BYTES / 3));
        assert!(served.await.unwrap().ends_with("\r\n\r\n"));
    }

    #[tokio::test]
    async fn 기본_전송기의_네트워크_실패는_응답의_error로_돌아온다() {
        let ctx = context();
        // A port nobody listens on any more.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let mut request = SendRequest {
            url,
            method: "GET".to_owned(),
            headers: Record::new(),
            body: None,
        };
        let response = ReqwestSender.send(&ctx, &request).await.unwrap();
        assert_eq!(response.status, 0);
        assert!(response.error.is_some());

        request.method = "NOT A METHOD".to_owned();
        let response = ReqwestSender.send(&ctx, &request).await.unwrap();
        assert_eq!(
            response.error.as_deref(),
            Some("'NOT A METHOD' is not a valid HTTP method.")
        );
    }

    // The scope and the fence are process-wide, so this is the only test that
    // sets them, and every other test here stays on hosts it keeps in scope.
    #[tokio::test]
    async fn 범위_밖의_대상은_보내지_않고_울타리가_쳐진_쓰기는_204로_답한다() {
        set_scope(&["shop.test", "127.0.0.1"]);
        let ctx = context_on(FakeBrowser::new(Ok(json!({ "items": [] }))));
        let mut request = SendRequest {
            url: "https://evil.example/api".to_owned(),
            method: "POST".to_owned(),
            headers: Record::new(),
            body: None,
        };
        let refused = ReqwestSender.send(&ctx, &request).await.unwrap_err();
        assert!(refused.starts_with("Out of scope: evil.example is not within the allowed scope"));

        // The in-page extract is held to the same scope.
        let data = shop(&ctx);
        let mut page = with_extract(&data, "page", CliEffect::Read);
        page.path = "/list".to_owned();
        let run = CliRun {
            base_url: "https://evil.example",
            writes: false,
            command: &page,
            inputs: &Record::new(),
        };
        let result = execute_cli(&ctx, run).await;
        assert!(
            result
                .error
                .is_some_and(|error| error.starts_with("Out of scope: evil.example"))
        );

        // Under the fence an in-scope write is answered without reaching the network.
        acquire_fence();
        request.url = "https://shop.test/api/items".to_owned();
        let fenced = ReqwestSender.send(&ctx, &request).await;
        release_fence();
        clear_scope();
        assert_eq!(
            fenced.map(|response| (response.status, response.error)),
            Ok((204, None))
        );
        assert!(
            get_fence_blocks()
                .iter()
                .any(|block| block.method == "POST" && block.url == "https://shop.test/api/items")
        );
    }
}
