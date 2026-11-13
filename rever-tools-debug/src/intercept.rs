//! Port of `src/main/mcp/tools/intercept.ts` — intercept rules, the requests they
//! hold, and replaying a captured request.
//!
//! The bookkeeping the TypeScript kept in `chrome-cdp.ts` module variables (the
//! rule list, its compiled patterns and the paused-request queue) lives in
//! [`InterceptState`]. Everything that has to reach the network stack goes
//! through [`InterceptEngine`], whose methods mirror the CDP `Fetch.*` commands
//! the TypeScript sent. `replay_request` needs no engine: it sends its own request.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use rever_formats::intercept_match::{
    CompiledRule, HasUrlPattern, compile_rules, find_matching_rule,
};
use rever_scope::scope::scope_block_for_url;
use rever_tools_api::tool::{BoxFuture, ToolDef, ToolRegistry, unsupported};
use rever_tools_api::utils::merge_replay_headers;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};

use crate::args::{int, text};
use crate::{lock, no_args, pretty, schema};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Let the request through and record it.
    Log,
    /// Cancel the request.
    Block,
    /// Hold the request for `intercept_continue` / `fulfill` / `fail`.
    Modify,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    #[default]
    Request,
    Response,
}

/// One rule as `intercept_list` prints it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterceptRule {
    pub id: String,
    /// A CDP wildcard pattern: `*` any run, `?` any one character, `\` escapes.
    pub url_pattern: String,
    pub mode: Mode,
    pub stage: Stage,
    /// Name and value, in the order the agent gave them.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "as_object")]
    pub modify_headers: Option<Vec<(String, String)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_body: Option<String>,
}

fn as_object<S: Serializer>(
    headers: &Option<Vec<(String, String)>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_map(headers.iter().flatten().map(|(name, value)| (name, value)))
}

impl HasUrlPattern for InterceptRule {
    fn url_pattern(&self) -> &str {
        &self.url_pattern
    }
}

/// A request held by a modify-mode rule, as `intercept_pending` prints it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PausedRequest {
    pub request_id: String,
    pub url: String,
    pub method: String,
    pub resource_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderEntry {
    pub name: String,
    pub value: String,
}

/// The parameters of `Fetch.continueRequest`; `None` leaves that part as it was.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContinueRequest {
    pub request_id: String,
    pub headers: Option<Vec<HeaderEntry>>,
    /// The new body, base64-encoded as CDP takes it.
    pub post_data: Option<String>,
    pub url: Option<String>,
    pub method: Option<String>,
}

/// The parameters of `Fetch.fulfillRequest`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FulfillRequest {
    pub request_id: String,
    pub response_code: i64,
    pub response_headers: Option<Vec<HeaderEntry>>,
    /// The response body, base64-encoded as CDP takes it.
    pub body: Option<String>,
}

/// What the intercept tools ask of the page's network stack. Every method has a
/// default that reports the operation as unsupported, so an engine implements
/// what it can. An `Err` is shown to the agent as the tool's error.
///
/// The engine is also what fills the queue: for each request it pauses it asks
/// [`InterceptState::matching_rule`], and holds the request with
/// [`InterceptState::hold`] when the rule's mode is [`Mode::Modify`].
pub trait InterceptEngine: Send + Sync {
    /// Whether there is a page to intercept. When there is none, rule changes
    /// are only recorded and the request tools answer "no active target".
    fn has_target(&self) -> bool {
        true
    }

    /// Start intercepting what `rules` match (CDP `Fetch.enable` with their
    /// patterns and stages), or stop when the list is empty (`Fetch.disable`).
    fn apply_rules<'a>(&'a self, _rules: &'a [InterceptRule]) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(unsupported("request interception")) })
    }

    fn continue_request<'a>(
        &'a self,
        _request: &'a ContinueRequest,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(unsupported("continuing a paused request")) })
    }

    fn fulfill_request<'a>(
        &'a self,
        _request: &'a FulfillRequest,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(unsupported("fulfilling a paused request")) })
    }

    /// `error_reason` is a CDP `Network.ErrorReason`, e.g. "BlockedByClient".
    fn fail_request<'a>(
        &'a self,
        _request_id: &'a str,
        _error_reason: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(unsupported("failing a paused request")) })
    }
}

/// The engine in place until the app sets one: nothing is supported.
pub struct NoInterceptEngine;

impl InterceptEngine for NoInterceptEngine {}

/// The intercept state shared by every call: `ctx.state::<InterceptState>()`.
#[derive(Default)]
pub struct InterceptState {
    // Compiled once per change so the per-request match never builds a pattern
    // from agent input.
    rules: Mutex<Vec<CompiledRule<InterceptRule>>>,
    // In the order the requests were paused.
    pending: Mutex<Vec<PausedRequest>>,
    engine: Mutex<Option<Arc<dyn InterceptEngine>>>,
}

impl InterceptState {
    pub fn rules(&self) -> Vec<InterceptRule> {
        lock(&self.rules)
            .iter()
            .map(|compiled| compiled.rule.clone())
            .collect()
    }

    pub fn set_rules(&self, rules: Vec<InterceptRule>) {
        self.update_rules(|current| *current = rules);
    }

    /// Changes the list under one lock, so concurrent calls cannot lose a rule.
    /// Returns what `change` returned and the list after it.
    fn update_rules<T>(
        &self,
        change: impl FnOnce(&mut Vec<InterceptRule>) -> T,
    ) -> (T, Vec<InterceptRule>) {
        let mut compiled = lock(&self.rules);
        let mut rules: Vec<InterceptRule> = std::mem::take(&mut *compiled)
            .into_iter()
            .map(|compiled| compiled.rule)
            .collect();
        let out = change(&mut rules);
        *compiled = compile_rules(rules.clone());
        (out, rules)
    }

    /// The first rule whose pattern matches the whole of `url`.
    pub fn matching_rule(&self, url: &str) -> Option<InterceptRule> {
        find_matching_rule(&lock(&self.rules), url).cloned()
    }

    pub fn pending(&self) -> Vec<PausedRequest> {
        lock(&self.pending).clone()
    }

    /// Queue a paused request. One paused again under the same id keeps its place.
    pub fn hold(&self, request: PausedRequest) {
        let mut pending = lock(&self.pending);
        match pending
            .iter_mut()
            .find(|held| held.request_id == request.request_id)
        {
            Some(held) => *held = request,
            None => pending.push(request),
        }
    }

    pub fn release(&self, request_id: &str) {
        lock(&self.pending).retain(|held| held.request_id != request_id);
    }

    /// The page went away: nothing it paused can be answered any more.
    pub fn clear_pending(&self) {
        lock(&self.pending).clear();
    }

    pub fn set_engine(&self, engine: Arc<dyn InterceptEngine>) {
        *lock(&self.engine) = Some(engine);
    }

    pub fn engine(&self) -> Arc<dyn InterceptEngine> {
        lock(&self.engine)
            .clone()
            .unwrap_or_else(|| Arc::new(NoInterceptEngine))
    }

    fn target(&self) -> Result<Arc<dyn InterceptEngine>, String> {
        let engine = self.engine();
        if !engine.has_target() {
            return Err("no active target".to_owned());
        }
        Ok(engine)
    }

    /// Re-apply the rules to the page, as `applyFetchIntercept` did: nothing to
    /// do without a page, and a failure is not the tool's failure — the rule
    /// change itself has happened.
    async fn apply(&self, rules: &[InterceptRule]) {
        let engine = self.engine();
        if engine.has_target() {
            let _ = engine.apply_rules(rules).await;
        }
    }
}

/// An argument that passed validation, as its Rust type.
fn arg<T: DeserializeOwned>(args: &Value, key: &str) -> Option<T> {
    serde_json::from_value(args.get(key)?.clone()).ok()
}

/// JavaScript truthiness of an optional string: absent and "" both mean "leave it".
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

fn header_list_schema(description: &str) -> Value {
    json!({
        "description": description,
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "value": { "type": "string" }
            },
            "required": ["name", "value"]
        }
    })
}

fn header_record_schema() -> Value {
    json!({
        "type": "object",
        "propertyNames": { "type": "string" },
        "additionalProperties": { "type": "string" }
    })
}

tool!(
    InterceptAdd,
    ToolDef::new(
        "intercept_add",
        "Add a network intercept rule. mode=log: auto-continues and records; mode=block: cancels the request; mode=modify: holds the request for manual intercept_continue/fulfill/fail.",
        schema(
            json!({
                "urlPattern": {
                    "type": "string",
                    "description": "CDP wildcard URL pattern, e.g. \"*/api/v1/*\""
                },
                "mode": {
                    "type": "string",
                    "enum": ["log", "block", "modify"],
                    "description": "Intercept mode"
                },
                "stage": {
                    "description": "Intercept stage (default: Request)",
                    "type": "string",
                    "enum": ["Request", "Response"]
                },
                "modifyHeaders": {
                    "description": "Headers to inject (modify mode)",
                    "type": "object",
                    "propertyNames": { "type": "string" },
                    "additionalProperties": { "type": "string" }
                },
                "replaceBody": {
                    "description": "Body to replace (modify mode)",
                    "type": "string"
                }
            }),
            &["urlPattern", "mode"]
        )
    ),
    |ctx, args| {
        let state = ctx.state::<InterceptState>();
        let modify_headers = args
            .get("modifyHeaders")
            .and_then(Value::as_object)
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
                    .collect()
            });
        let rule = InterceptRule {
            id: uuid::Uuid::new_v4().to_string(),
            url_pattern: text(args, "urlPattern").unwrap_or_default().to_owned(),
            mode: arg(args, "mode").ok_or("mode is required")?,
            stage: arg(args, "stage").unwrap_or_default(),
            modify_headers,
            replace_body: text(args, "replaceBody").map(str::to_owned),
        };
        let (_, rules) = state.update_rules(|rules| rules.push(rule.clone()));
        state.apply(&rules).await;
        Ok(pretty(&json!({
            "id": rule.id,
            "urlPattern": rule.url_pattern,
            "mode": rule.mode,
            "stage": rule.stage
        })))
    }
);

tool!(
    InterceptList,
    ToolDef::new(
        "intercept_list",
        "List all active intercept rules.",
        no_args()
    ),
    |ctx, _args| { Ok(pretty(&ctx.state::<InterceptState>().rules())) }
);

tool!(
    InterceptRemove,
    ToolDef::new(
        "intercept_remove",
        "Remove an intercept rule by ID.",
        schema(
            json!({
                "id": {
                    "type": "string",
                    "description": "Rule ID from intercept_list"
                }
            }),
            &["id"]
        )
    ),
    |ctx, args| {
        let state = ctx.state::<InterceptState>();
        let id = text(args, "id").unwrap_or_default();
        let (removed, rules) = state.update_rules(|rules| {
            let before = rules.len();
            rules.retain(|rule| rule.id != id);
            rules.len() != before
        });
        if !removed {
            return Err(format!("unknown rule id: {id}"));
        }
        state.apply(&rules).await;
        Ok(format!("rule {id} removed"))
    }
);

tool!(
    InterceptPending,
    ToolDef::new(
        "intercept_pending",
        "List requests currently paused by a modify-mode intercept rule.",
        no_args()
    ),
    |ctx, _args| { Ok(pretty(&ctx.state::<InterceptState>().pending())) }
);

tool!(
    InterceptContinue,
    ToolDef::new(
        "intercept_continue",
        "Continue a paused request (optionally overriding headers, body, URL, or method).",
        schema(
            json!({
                "requestId": {
                    "type": "string",
                    "description": "Fetch requestId from intercept_pending"
                },
                "headers": header_list_schema("Override request headers"),
                "postData": {
                    "description": "Override request body",
                    "type": "string"
                },
                "url": {
                    "description": "Override URL",
                    "type": "string"
                },
                "method": {
                    "description": "Override HTTP method",
                    "type": "string"
                }
            }),
            &["requestId"]
        )
    ),
    |ctx, args| {
        let url = text(args, "url");
        // A URL override can redirect the continued request to another host —
        // enforce scope on it (no-op until a scope is set).
        if let Some(block) = scope_block_for_url(url) {
            return Err(block);
        }
        let state = ctx.state::<InterceptState>();
        let engine = state.target()?;
        let request_id = text(args, "requestId").unwrap_or_default();
        let request = ContinueRequest {
            request_id: request_id.to_owned(),
            headers: arg(args, "headers"),
            post_data: non_empty(text(args, "postData")).map(|body| BASE64.encode(body)),
            url: non_empty(url).map(str::to_owned),
            method: non_empty(text(args, "method")).map(str::to_owned),
        };
        engine.continue_request(&request).await?;
        state.release(request_id);
        Ok(format!("continued {request_id}"))
    }
);

tool!(
    InterceptFulfill,
    ToolDef::new(
        "intercept_fulfill",
        "Fulfill a paused request with a custom response.",
        schema(
            json!({
                "requestId": {
                    "type": "string",
                    "description": "Fetch requestId from intercept_pending"
                },
                "responseCode": {
                    "type": "integer",
                    "minimum": -9007199254740991i64,
                    "maximum": 9007199254740991i64,
                    "description": "HTTP status code"
                },
                "headers": header_list_schema("Response headers"),
                "body": {
                    "description": "Response body (will be base64-encoded)",
                    "type": "string"
                }
            }),
            &["requestId", "responseCode"]
        )
    ),
    |ctx, args| {
        let state = ctx.state::<InterceptState>();
        let engine = state.target()?;
        let request_id = text(args, "requestId").unwrap_or_default();
        let response_code = int(args, "responseCode").unwrap_or_default();
        let request = FulfillRequest {
            request_id: request_id.to_owned(),
            response_code,
            response_headers: arg(args, "headers"),
            body: non_empty(text(args, "body")).map(|body| BASE64.encode(body)),
        };
        engine.fulfill_request(&request).await?;
        state.release(request_id);
        Ok(format!("fulfilled {request_id} with {response_code}"))
    }
);

tool!(
    InterceptFail,
    ToolDef::new(
        "intercept_fail",
        "Fail (abort) a paused request.",
        schema(
            json!({
                "requestId": {
                    "type": "string",
                    "description": "Fetch requestId from intercept_pending"
                },
                "errorReason": {
                    "description": "CDP error reason (default: BlockedByClient)",
                    "type": "string"
                }
            }),
            &["requestId"]
        )
    ),
    |ctx, args| {
        let state = ctx.state::<InterceptState>();
        let engine = state.target()?;
        let request_id = text(args, "requestId").unwrap_or_default();
        let error_reason = text(args, "errorReason").unwrap_or("BlockedByClient");
        engine.fail_request(request_id, error_reason).await?;
        state.release(request_id);
        Ok(format!("failed {request_id}"))
    }
);

#[derive(Debug, Default, Deserialize)]
struct ReplayOverrides {
    url: Option<String>,
    method: Option<String>,
    headers: Option<serde_json::Map<String, Value>>,
    body: Option<String>,
}

/// Override headers under lowercase names, folded in the order given: when both
/// "Cookie" and "cookie" are sent, the later one wins, as in the TypeScript. The
/// merge only sees sorted names and could not tell which came last.
fn fold_override_headers(headers: &serde_json::Map<String, Value>) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| Some((name.to_lowercase(), value.as_str()?.to_owned())))
        .collect()
}

tool!(
    ReplayRequest,
    ToolDef::new(
        "replay_request",
        "Replay a captured request via Node fetch (independent of the browser). Cookies are pulled from the page and injected automatically, but explicit header overrides win — set a header to \"\" to remove it. For a truly unauthenticated replay you must blank BOTH {\"cookie\":\"\",\"authorization\":\"\"}: the page cookie may itself authenticate the request (e.g. a JWT extracted from a cookie server-side). The result lists `credentialsSent` so you can see what was actually attached.",
        schema(
            json!({
                "requestId": {
                    "type": "string",
                    "description": "requestId to replay"
                },
                "overrides": {
                    "description": "Optional overrides for the replayed request (strict: unknown keys error)",
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" },
                        "method": { "type": "string" },
                        "headers": header_record_schema(),
                        "body": { "type": "string" }
                    },
                    "additionalProperties": false
                }
            }),
            &["requestId"]
        )
    ),
    |ctx, args| {
        let request_id = text(args, "requestId").unwrap_or_default();
        let overrides: ReplayOverrides = arg(args, "overrides").unwrap_or_default();
        // Copied out: the store's lock is not held while the request is in flight.
        let entry = lock(&ctx.traffic).get(request_id).cloned();
        let Some(entry) = entry else {
            return Err(format!("unknown requestId: {request_id}"));
        };
        let url = overrides.url.as_deref().unwrap_or(&entry.url);
        // This request does not go through the browser, so enforce scope here
        // too — the override URL would otherwise also receive the page's cookies.
        if let Some(block) = scope_block_for_url(Some(url)) {
            return Err(block);
        }

        // Collect cookies for the target URL. Best effort: a URL that does not
        // parse fails below, and a browser that cannot list cookies sends none.
        let mut cookie_header = String::new();
        if let Ok(parsed) = reqwest::Url::parse(url)
            && let Ok(cookies) = ctx
                .browser
                .cookies(&parsed.origin().ascii_serialization())
                .await
        {
            let pairs: Vec<String> = cookies
                .iter()
                .map(|cookie| format!("{}={}", cookie.name, cookie.value))
                .collect();
            cookie_header = pairs.join("; ");
        }

        let override_headers = overrides.headers.as_ref().map(fold_override_headers);
        let headers = merge_replay_headers(
            entry.request_headers.as_ref(),
            &cookie_header,
            override_headers.as_ref(),
        );
        // Surface which credential headers survived the merge — an auto-injected
        // page cookie can silently authenticate a replay the agent believed was
        // credential-stripped (cookie-extracted JWTs do exactly this).
        let credentials_sent: Vec<&str> = ["cookie", "authorization", "proxy-authorization"]
            .into_iter()
            .filter(|name| headers.contains_key(*name))
            .collect();

        let sent_method = overrides.method.as_deref().unwrap_or(&entry.method);
        let sent_body = overrides
            .body
            .as_deref()
            .or(entry.request_post_data.as_deref())
            .unwrap_or("");
        let response = node_fetch(url, sent_method, &headers, sent_body).await?;
        let body = utf16_prefix(&response.body, 10_000);
        Ok(pretty(&json!({
            "status": response.status,
            "statusText": response.status_text,
            "credentialsSent": credentials_sent,
            // What actually went out — a silently-unchanged replay (e.g.
            // overrides in the wrong shape) must be visible to the caller.
            "sent": { "method": sent_method, "body": utf16_prefix(sent_body, 200) },
            "headers": response.headers,
            "body": body,
            "truncated": body.len() < response.body.len()
        })))
    }
);

/// `text.slice(0, max_units)`: the limit counts UTF-16 code units.
// shortcut: a limit that lands inside a surrogate pair drops the whole character,
// where JavaScript keeps half of it (printed as a `\ud83d` escape); a Rust string
// cannot hold the half.
fn utf16_prefix(text: &str, max_units: usize) -> &str {
    let mut units = 0;
    for (index, ch) in text.char_indices() {
        units += ch.len_utf16();
        if units > max_units {
            return text.get(..index).unwrap_or(text);
        }
    }
    text
}

struct Fetched {
    status: u16,
    status_text: String,
    /// What `Headers.forEach` visits: lowercase names in sorted order.
    headers: BTreeMap<String, String>,
    body: String,
}

fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

/// Sends a request the way Node's `fetch(url, { method, headers, body })` does,
/// and fails with the message `fetch` rejects with: the agent reads that text.
async fn node_fetch(
    url: &str,
    method: &str,
    headers: &BTreeMap<String, String>,
    body: &str,
) -> Result<Fetched, String> {
    let fetch_failed = || "fetch failed".to_owned();

    let parsed = reqwest::Url::parse(url).map_err(|_| format!("Failed to parse URL from {url}"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "Request cannot be constructed from a URL that includes credentials: {url}"
        ));
    }

    if !is_token(method) {
        return Err(format!("'{method}' is not a valid HTTP method."));
    }
    let upper = method.to_ascii_uppercase();
    if matches!(upper.as_str(), "CONNECT" | "TRACE" | "TRACK") {
        return Err(format!("'{method}' HTTP method is unsupported."));
    }
    // fetch upper-cases only the methods it knows; "patch" goes out as written.
    let known = ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"].contains(&upper.as_str());
    let method = if known { upper.as_str() } else { method };
    let wire_method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| format!("'{method}' is not a valid HTTP method."))?;

    // Header names and values are byte strings: fetch refuses anything outside
    // Latin-1 before it looks at any single header.
    for part in headers.iter().flat_map(|(name, value)| [name, value]) {
        if let Some((index, unit)) = part.encode_utf16().enumerate().find(|(_, u)| *u > 255) {
            return Err(format!(
                "Cannot convert argument to a ByteString because the character at index {index} has a value of {unit} which is greater than 255."
            ));
        }
    }
    let mut header_map = HeaderMap::new();
    for (name, value) in headers {
        let value = value.trim_matches(['\t', '\n', '\r', ' ']);
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("Headers.append: \"{name}\" is an invalid header name."))?;
        if value.contains(['\0', '\r', '\n']) {
            return Err(format!(
                "Headers.append: \"{value}\" is an invalid header value."
            ));
        }
        let latin1: Vec<u8> = value
            .chars()
            .map(|ch| u8::try_from(u32::from(ch)).unwrap_or(b'?'))
            .collect();
        // Other control characters pass fetch's check but cannot be sent.
        let header_value = HeaderValue::from_bytes(&latin1).map_err(|_| fetch_failed())?;
        header_map.insert(header_name, header_value);
    }

    let is_get_or_head = method == "GET" || method == "HEAD";
    if !body.is_empty() && is_get_or_head {
        return Err("Request with GET/HEAD method cannot have body.".to_owned());
    }
    // The client writes content-length itself. fetch refuses a captured one that
    // contradicts the body; sending it would produce a malformed request.
    if let Some(length) = header_map.remove(CONTENT_LENGTH) {
        let declared = length.to_str().ok().and_then(|l| l.parse::<usize>().ok());
        if !body.is_empty() && declared != Some(body.len()) {
            return Err(fetch_failed());
        }
    }
    // What Node's fetch sends on its own; a server or WAF may key on them.
    let accept_encoding = match parsed.scheme() {
        "https" => "br, gzip, deflate",
        _ => "gzip, deflate",
    };
    for (name, value) in [
        ("accept", "*/*"),
        ("accept-language", "*"),
        ("sec-fetch-mode", "cors"),
        ("user-agent", "node"),
        ("accept-encoding", accept_encoding),
    ] {
        header_map
            .entry(name)
            .or_insert(HeaderValue::from_static(value));
    }
    if !body.is_empty() {
        header_map
            .entry(CONTENT_TYPE)
            .or_insert(HeaderValue::from_static("text/plain;charset=UTF-8"));
    }

    let client = reqwest::Client::builder()
        // Node's fetch follows up to 20 redirects, ignores the proxy environment
        // variables and speaks HTTP/1.1.
        .redirect(reqwest::redirect::Policy::limited(20))
        .no_proxy()
        .http1_only()
        .build()
        .map_err(|_| fetch_failed())?;
    let mut request = client.request(wire_method, parsed).headers(header_map);
    if !body.is_empty() {
        request = request.body(body.to_owned());
    } else if matches!(method, "POST" | "PUT" | "PATCH") {
        // Node sends this for these methods without a body; the client would
        // send no length at all, and some servers answer 411 to that.
        request = request.header(CONTENT_LENGTH, "0");
    }
    let response = request.send().await.map_err(|_| fetch_failed())?;

    let status = response.status();
    let mut response_headers = BTreeMap::new();
    for (name, value) in response.headers() {
        // Header bytes are Latin-1 to fetch.
        let value: String = value
            .as_bytes()
            .iter()
            .map(|&byte| char::from(byte))
            .collect();
        match response_headers.entry(name.as_str().to_owned()) {
            Entry::Vacant(slot) => {
                slot.insert(value);
            }
            // forEach visits every set-cookie on its own, so the last one is
            // what the result object keeps; other repeats arrive joined.
            Entry::Occupied(mut slot) if name == reqwest::header::SET_COOKIE => {
                slot.insert(value);
            }
            Entry::Occupied(mut slot) => {
                slot.get_mut().push_str(", ");
                slot.get_mut().push_str(&value);
            }
        }
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "terminated".to_owned())?;
    // `Response.text()` decodes UTF-8 and drops a leading byte-order mark.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    Ok(Fetched {
        status: status.as_u16(),
        // shortcut: the standard phrase for the status; a server's own wording
        // ("418 Custom Reason") is not exposed by the HTTP client.
        status_text: status.canonical_reason().unwrap_or("").to_owned(),
        headers: response_headers,
        body: String::from_utf8_lossy(bytes).into_owned(),
    })
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(InterceptAdd);
    registry.register(InterceptList);
    registry.register(InterceptRemove);
    registry.register(InterceptPending);
    registry.register(InterceptContinue);
    registry.register(InterceptFulfill);
    registry.register(InterceptFail);
    registry.register(ReplayRequest);
}

// Each test runs on its own single-threaded runtime, so holding the scope guard
// across an await cannot block another task of the same runtime.
#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::sync::{MutexGuard, PoisonError};

    use rever_core::traffic::RequestPatch;
    use rever_scope::scope::{clear_scope, set_scope};
    use rever_tools_api::tool::{Browser, Cookie, ToolContext};

    use super::*;
    use crate::test_support::{call, call_json};

    /// The scope allowlist is one per process. Every test that sets it, or calls
    /// a tool that reads it, holds this for its whole run.
    fn scope_guard() -> MutexGuard<'static, ()> {
        static SCOPE: Mutex<()> = Mutex::new(());
        let guard = SCOPE.lock().unwrap_or_else(PoisonError::into_inner);
        clear_scope();
        guard
    }

    /// Records what the tools ask for and answers from its fields.
    #[derive(Default)]
    struct FakeEngine {
        calls: Mutex<Vec<String>>,
        detached: bool,
        /// Every operation fails with this message.
        fail: Option<&'static str>,
    }

    impl FakeEngine {
        fn record(&self, call: String) -> Result<(), String> {
            lock(&self.calls).push(call);
            self.fail.map_or(Ok(()), |message| Err(message.to_owned()))
        }

        fn calls(&self) -> Vec<String> {
            lock(&self.calls).clone()
        }
    }

    impl InterceptEngine for FakeEngine {
        fn has_target(&self) -> bool {
            !self.detached
        }

        fn apply_rules<'a>(
            &'a self,
            rules: &'a [InterceptRule],
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                let patterns: Vec<String> = rules
                    .iter()
                    .map(|rule| format!("{}@{:?}", rule.url_pattern, rule.stage))
                    .collect();
                self.record(format!("apply:[{}]", patterns.join(",")))
            })
        }

        fn continue_request<'a>(
            &'a self,
            request: &'a ContinueRequest,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move { self.record(format!("continue:{request:?}")) })
        }

        fn fulfill_request<'a>(
            &'a self,
            request: &'a FulfillRequest,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move { self.record(format!("fulfill:{request:?}")) })
        }

        fn fail_request<'a>(
            &'a self,
            request_id: &'a str,
            error_reason: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move { self.record(format!("fail:{request_id}:{error_reason}")) })
        }
    }

    fn context(engine: FakeEngine) -> (Arc<FakeEngine>, ToolContext) {
        let engine = Arc::new(engine);
        let context = ToolContext::for_tests();
        context.state::<InterceptState>().set_engine(engine.clone());
        (engine, context)
    }

    fn rule(id: &str, pattern: &str, mode: Mode) -> InterceptRule {
        InterceptRule {
            id: id.to_owned(),
            url_pattern: pattern.to_owned(),
            mode,
            stage: Stage::Request,
            modify_headers: None,
            replace_body: None,
        }
    }

    fn held(id: &str, url: &str) -> PausedRequest {
        PausedRequest {
            request_id: id.to_owned(),
            url: url.to_owned(),
            method: "POST".to_owned(),
            resource_type: "XHR".to_owned(),
        }
    }

    // --- rules ---

    #[tokio::test]
    async fn intercept_add는_규칙을_기록하고_엔진에_다시_적용한다() {
        let (engine, context) = context(FakeEngine::default());
        let args = json!({ "urlPattern": "*/api/v1/*", "mode": "modify" });
        let text = call(&InterceptAdd, &context, args).await.unwrap();

        let rules = context.state::<InterceptState>().rules();
        assert_eq!(rules.len(), 1);
        let id = &rules[0].id;
        // randomUUID: 36 characters, version 4.
        assert_eq!(id.len(), 36);
        assert_eq!(id.chars().nth(14), Some('4'));
        // The stage defaults to Request.
        assert_eq!(
            rules[0],
            InterceptRule {
                id: id.clone(),
                ..rule("", "*/api/v1/*", Mode::Modify)
            }
        );
        assert_eq!(
            text,
            format!(
                "{{\n  \"id\": \"{id}\",\n  \"urlPattern\": \"*/api/v1/*\",\n  \"mode\": \"modify\",\n  \"stage\": \"Request\"\n}}"
            )
        );

        let args = json!({ "urlPattern": "*.png", "mode": "block", "stage": "Response" });
        let second = call_json(&InterceptAdd, &context, args).await;
        assert_eq!(second["stage"], json!("Response"));
        assert_eq!(
            engine.calls(),
            [
                "apply:[*/api/v1/*@Request]",
                "apply:[*/api/v1/*@Request,*.png@Response]"
            ]
        );
    }

    #[tokio::test]
    async fn intercept_add는_엔진_적용이_실패해도_규칙을_남기고_성공으로_답한다() {
        let (engine, context) = context(FakeEngine {
            fail: Some("Fetch.enable failed"),
            ..FakeEngine::default()
        });
        let args = json!({ "urlPattern": "*", "mode": "log" });
        assert!(call(&InterceptAdd, &context, args).await.is_ok());
        assert_eq!(engine.calls(), ["apply:[*@Request]"]);
        assert_eq!(context.state::<InterceptState>().rules().len(), 1);
    }

    #[tokio::test]
    async fn 대상_페이지가_없으면_규칙은_기록만_하고_엔진을_부르지_않는다() {
        let (engine, context) = context(FakeEngine {
            detached: true,
            ..FakeEngine::default()
        });
        let args = json!({ "urlPattern": "*", "mode": "log" });
        let added = call_json(&InterceptAdd, &context, args).await;
        let id = added["id"].as_str().unwrap();
        assert_eq!(context.state::<InterceptState>().rules().len(), 1);
        assert_eq!(
            call(&InterceptRemove, &context, json!({ "id": id })).await,
            Ok(format!("rule {id} removed"))
        );
        assert!(engine.calls().is_empty());
    }

    #[tokio::test]
    async fn intercept_list는_없는_필드를_빼고_추가한_순서대로_보여준다() {
        let context = ToolContext::for_tests();
        assert_eq!(
            call(&InterceptList, &context, json!({})).await.unwrap(),
            "[]"
        );

        let args = json!({
            "urlPattern": "*/login",
            "mode": "modify",
            "modifyHeaders": { "x-b": "2", "x-a": "1" },
            "replaceBody": ""
        });
        call(&InterceptAdd, &context, args).await.unwrap();
        let args = json!({ "urlPattern": "*.js", "mode": "log" });
        call(&InterceptAdd, &context, args).await.unwrap();

        let rules = context.state::<InterceptState>().rules();
        assert_eq!(
            call(&InterceptList, &context, Value::Null).await.unwrap(),
            format!(
                "[\n  {{\n    \"id\": \"{}\",\n    \"urlPattern\": \"*/login\",\n    \"mode\": \"modify\",\n    \"stage\": \"Request\",\n    \"modifyHeaders\": {{\n      \"x-b\": \"2\",\n      \"x-a\": \"1\"\n    }},\n    \"replaceBody\": \"\"\n  }},\n  {{\n    \"id\": \"{}\",\n    \"urlPattern\": \"*.js\",\n    \"mode\": \"log\",\n    \"stage\": \"Request\"\n  }}\n]",
                rules[0].id, rules[1].id
            )
        );
    }

    #[tokio::test]
    async fn intercept_remove는_규칙을_지우고_남은_규칙을_다시_적용한다() {
        let (engine, context) = context(FakeEngine::default());
        let state = context.state::<InterceptState>();
        state.set_rules(vec![
            rule("a", "*/a", Mode::Log),
            rule("b", "*/b", Mode::Block),
        ]);

        assert_eq!(
            call(&InterceptRemove, &context, json!({ "id": "zz" })).await,
            Err("unknown rule id: zz".to_owned())
        );
        assert!(engine.calls().is_empty());
        assert_eq!(state.rules().len(), 2);

        assert_eq!(
            call(&InterceptRemove, &context, json!({ "id": "a" })).await,
            Ok("rule a removed".to_owned())
        );
        assert_eq!(state.rules(), [rule("b", "*/b", Mode::Block)]);
        // The last rule gone: the engine is told to stop with an empty list.
        call(&InterceptRemove, &context, json!({ "id": "b" }))
            .await
            .unwrap();
        assert_eq!(engine.calls(), ["apply:[*/b@Request]", "apply:[]"]);
        // The removed rule no longer matches anything.
        assert_eq!(state.matching_rule("https://x.test/a"), None);
    }

    #[test]
    fn 규칙은_url_전체에_먼저_맞는_것이_이긴다() {
        let state = InterceptState::default();
        assert_eq!(state.matching_rule("https://x.test/api/v1/users"), None);
        state.set_rules(vec![
            rule("first", "*/api/v1/*", Mode::Modify),
            rule("second", "*/api/*", Mode::Block),
            rule("literal", "https://x.test/a.b?", Mode::Log),
        ]);
        let matched = |url: &str| state.matching_rule(url).map(|rule| rule.id);
        assert_eq!(
            matched("https://x.test/api/v1/users").as_deref(),
            Some("first")
        );
        assert_eq!(
            matched("https://x.test/api/v2/users").as_deref(),
            Some("second")
        );
        // Anchored at both ends; `.` is literal and `?` is exactly one character.
        assert_eq!(matched("https://x.test/a.bc").as_deref(), Some("literal"));
        assert_eq!(matched("https://x.test/aXbc"), None);
        assert_eq!(matched("https://x.test/a.b"), None);
        assert_eq!(matched("https://x.test/other"), None);

        // Replacing the list replaces what matches.
        state.set_rules(vec![rule("only", "*", Mode::Log)]);
        assert_eq!(matched("https://x.test/other").as_deref(), Some("only"));
    }

    // --- paused requests ---

    #[tokio::test]
    async fn intercept_pending은_멈춘_요청을_멈춘_순서대로_보여준다() {
        let context = ToolContext::for_tests();
        assert_eq!(
            call(&InterceptPending, &context, json!({})).await.unwrap(),
            "[]"
        );
        let state = context.state::<InterceptState>();
        state.hold(held("interception-1", "https://x.test/a"));
        state.hold(held("interception-2", "https://x.test/b"));
        // Paused again under the same id: updated in place, like Map.set.
        state.hold(PausedRequest {
            method: "PUT".to_owned(),
            ..held("interception-1", "https://x.test/a2")
        });
        assert_eq!(
            call(&InterceptPending, &context, Value::Null)
                .await
                .unwrap(),
            "[\n  {\n    \"requestId\": \"interception-1\",\n    \"url\": \"https://x.test/a2\",\n    \"method\": \"PUT\",\n    \"resourceType\": \"XHR\"\n  },\n  {\n    \"requestId\": \"interception-2\",\n    \"url\": \"https://x.test/b\",\n    \"method\": \"POST\",\n    \"resourceType\": \"XHR\"\n  }\n]"
        );
        state.clear_pending();
        assert!(state.pending().is_empty());
    }

    #[tokio::test]
    async fn intercept_continue는_덮어쓸_것만_엔진에_넘기고_대기열에서_뺀다() {
        let _scope = scope_guard();
        let (engine, context) = context(FakeEngine::default());
        let state = context.state::<InterceptState>();
        state.hold(held("r1", "https://x.test/a"));
        state.hold(held("r2", "https://x.test/b"));

        assert_eq!(
            call(&InterceptContinue, &context, json!({ "requestId": "r1" })).await,
            Ok("continued r1".to_owned())
        );
        assert_eq!(state.pending(), [held("r2", "https://x.test/b")]);

        // The body goes out base64-encoded (Buffer.from("héllo 한") in Node);
        // extra keys of a header entry are dropped.
        let args = json!({
            "requestId": "r2",
            "headers": [{ "name": "x-a", "value": "1", "extra": true }],
            "postData": "héllo 한",
            "url": "https://x.test/c",
            "method": "PUT"
        });
        assert_eq!(
            call(&InterceptContinue, &context, args).await,
            Ok("continued r2".to_owned())
        );
        assert!(state.pending().is_empty());
        assert_eq!(
            engine.calls(),
            [
                r#"continue:ContinueRequest { request_id: "r1", headers: None, post_data: None, url: None, method: None }"#,
                r#"continue:ContinueRequest { request_id: "r2", headers: Some([HeaderEntry { name: "x-a", value: "1" }]), post_data: Some("aMOpbGxvIO2VnA=="), url: Some("https://x.test/c"), method: Some("PUT") }"#
            ]
        );
    }

    #[tokio::test]
    async fn intercept_continue는_빈_문자열_덮어쓰기는_보내지_않고_빈_헤더_목록은_보낸다() {
        let _scope = scope_guard();
        let (engine, context) = context(FakeEngine::default());
        let args =
            json!({ "requestId": "r1", "headers": [], "postData": "", "url": "", "method": "" });
        call(&InterceptContinue, &context, args).await.unwrap();
        assert_eq!(
            engine.calls(),
            [
                r#"continue:ContinueRequest { request_id: "r1", headers: Some([]), post_data: None, url: None, method: None }"#
            ]
        );
    }

    #[tokio::test]
    async fn intercept_continue는_범위_밖_url로_바꾸지_못한다() {
        let _scope = scope_guard();
        let (engine, context) = context(FakeEngine::default());
        let state = context.state::<InterceptState>();
        state.hold(held("r1", "https://example.com/a"));
        set_scope(&["example.com"]);

        let args = json!({ "requestId": "r1", "url": "https://evil.test/steal" });
        assert_eq!(
            call(&InterceptContinue, &context, args).await,
            Err("Out of scope: evil.test is not within the allowed scope [example.com]. Add it with scope_add if this target is intended.".to_owned())
        );
        // Still held, and the engine never heard of it.
        assert_eq!(state.pending().len(), 1);
        assert!(engine.calls().is_empty());

        // In scope, and with no URL override at all, it goes through.
        let args = json!({ "requestId": "r1", "url": "https://api.example.com/b" });
        assert!(call(&InterceptContinue, &context, args).await.is_ok());
        assert!(
            call(&InterceptContinue, &context, json!({ "requestId": "r2" }))
                .await
                .is_ok()
        );
        clear_scope();
    }

    #[tokio::test]
    async fn intercept_fulfill은_응답을_엔진에_넘기고_상태_코드를_알린다() {
        let (engine, context) = context(FakeEngine::default());
        let state = context.state::<InterceptState>();
        state.hold(held("r1", "https://x.test/a"));
        let args = json!({
            "requestId": "r1",
            "responseCode": 200.0,
            "headers": [{ "name": "content-type", "value": "application/json" }],
            "body": "{\"ok\":true}"
        });
        assert_eq!(
            call(&InterceptFulfill, &context, args).await,
            Ok("fulfilled r1 with 200".to_owned())
        );
        assert!(state.pending().is_empty());

        // No headers and an empty body are left out.
        let args = json!({ "requestId": "r2", "responseCode": 404, "body": "" });
        assert_eq!(
            call(&InterceptFulfill, &context, args).await,
            Ok("fulfilled r2 with 404".to_owned())
        );
        assert_eq!(
            engine.calls(),
            [
                r#"fulfill:FulfillRequest { request_id: "r1", response_code: 200, response_headers: Some([HeaderEntry { name: "content-type", value: "application/json" }]), body: Some("eyJvayI6dHJ1ZX0=") }"#,
                r#"fulfill:FulfillRequest { request_id: "r2", response_code: 404, response_headers: None, body: None }"#
            ]
        );
    }

    #[tokio::test]
    async fn intercept_fail은_기본_사유로_blocked_by_client를_쓴다() {
        let (engine, context) = context(FakeEngine::default());
        let state = context.state::<InterceptState>();
        state.hold(held("r1", "https://x.test/a"));
        assert_eq!(
            call(&InterceptFail, &context, json!({ "requestId": "r1" })).await,
            Ok("failed r1".to_owned())
        );
        assert!(state.pending().is_empty());
        let args = json!({ "requestId": "r2", "errorReason": "TimedOut" });
        call(&InterceptFail, &context, args).await.unwrap();
        // Only an absent reason gets the default; an empty one is sent as it is.
        let args = json!({ "requestId": "r3", "errorReason": "" });
        call(&InterceptFail, &context, args).await.unwrap();
        assert_eq!(
            engine.calls(),
            ["fail:r1:BlockedByClient", "fail:r2:TimedOut", "fail:r3:"]
        );
    }

    #[tokio::test]
    async fn 엔진이_실패하면_에러_문구를_돌려주고_요청은_대기열에_남는다() {
        let _scope = scope_guard();
        let (_, context) = context(FakeEngine {
            fail: Some("Invalid InterceptionId."),
            ..FakeEngine::default()
        });
        let state = context.state::<InterceptState>();
        state.hold(held("r1", "https://x.test/a"));
        let failed = Err("Invalid InterceptionId.".to_owned());

        assert_eq!(
            call(&InterceptContinue, &context, json!({ "requestId": "r1" })).await,
            failed
        );
        let args = json!({ "requestId": "r1", "responseCode": 200 });
        assert_eq!(call(&InterceptFulfill, &context, args).await, failed);
        assert_eq!(
            call(&InterceptFail, &context, json!({ "requestId": "r1" })).await,
            failed
        );
        assert_eq!(state.pending().len(), 1);
    }

    #[tokio::test]
    async fn 대상_페이지가_없으면_멈춘_요청_도구는_no_active_target으로_답한다() {
        let _scope = scope_guard();
        let (engine, context) = context(FakeEngine {
            detached: true,
            ..FakeEngine::default()
        });
        let no_target = Err("no active target".to_owned());
        assert_eq!(
            call(&InterceptContinue, &context, json!({ "requestId": "r1" })).await,
            no_target
        );
        let args = json!({ "requestId": "r1", "responseCode": 200 });
        assert_eq!(call(&InterceptFulfill, &context, args).await, no_target);
        assert_eq!(
            call(&InterceptFail, &context, json!({ "requestId": "r1" })).await,
            no_target
        );
        assert!(engine.calls().is_empty());
    }

    #[tokio::test]
    async fn 엔진을_꽂지_않으면_규칙은_기록되지만_멈춘_요청_도구는_지원하지_않는다고_알린다() {
        let _scope = scope_guard();
        let context = ToolContext::for_tests();
        let state = context.state::<InterceptState>();
        let args = json!({ "urlPattern": "*", "mode": "modify" });
        let added = call_json(&InterceptAdd, &context, args).await;
        assert_eq!(state.rules().len(), 1);
        state.hold(held("r1", "https://x.test/a"));

        assert_eq!(
            call(&InterceptContinue, &context, json!({ "requestId": "r1" })).await,
            Err(
                "continuing a paused request is not supported by this browser engine yet"
                    .to_owned()
            )
        );
        let args = json!({ "requestId": "r1", "responseCode": 200 });
        assert_eq!(
            call(&InterceptFulfill, &context, args).await,
            Err(unsupported("fulfilling a paused request"))
        );
        assert_eq!(
            call(&InterceptFail, &context, json!({ "requestId": "r1" })).await,
            Err(unsupported("failing a paused request"))
        );
        assert_eq!(state.pending().len(), 1);
        assert_eq!(
            NoInterceptEngine.apply_rules(&state.rules()).await,
            Err(unsupported("request interception"))
        );

        let id = added["id"].as_str().unwrap();
        assert!(
            call(&InterceptRemove, &context, json!({ "id": id }))
                .await
                .is_ok()
        );
        assert!(state.rules().is_empty());
    }

    // --- replay_request ---

    /// A browser whose only ability is listing cookies.
    #[derive(Default)]
    struct CookieBrowser {
        cookies: Vec<(&'static str, &'static str)>,
        asked: Mutex<Vec<String>>,
    }

    impl Browser for CookieBrowser {
        fn cookies<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Vec<Cookie>, String>> {
            Box::pin(async move {
                lock(&self.asked).push(url.to_owned());
                Ok(self
                    .cookies
                    .iter()
                    .map(|(name, value)| Cookie {
                        name: (*name).to_owned(),
                        value: (*value).to_owned(),
                        ..Cookie::default()
                    })
                    .collect())
            })
        }
    }

    fn cookie_context(
        cookies: &[(&'static str, &'static str)],
    ) -> (Arc<CookieBrowser>, ToolContext) {
        let browser = Arc::new(CookieBrowser {
            cookies: cookies.to_vec(),
            asked: Mutex::default(),
        });
        let context = ToolContext::new(Arc::default(), browser.clone(), std::env::temp_dir());
        (browser, context)
    }

    fn capture(context: &ToolContext, id: &str, url: &str, method: &str) -> RequestPatch {
        let patch = RequestPatch {
            request_id: id.to_owned(),
            url: Some(url.to_owned()),
            method: Some(method.to_owned()),
            ..RequestPatch::default()
        };
        lock(&context.traffic).upsert(patch.clone(), 1);
        patch
    }

    /// Serves one HTTP response on a loopback port; returns the origin and a
    /// channel that yields the request the client sent (head and body).
    fn serve_once(response: Vec<u8>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let (seen, request) = mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            let mut received = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let text = String::from_utf8_lossy(&received).into_owned();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length: ")?
                                .parse()
                                .ok()
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break;
                    }
                }
                match socket.read(&mut chunk) {
                    Ok(n) if n > 0 => received.extend_from_slice(&chunk[..n]),
                    _ => break,
                }
            }
            let _ = seen.send(String::from_utf8_lossy(&received).into_owned());
            let _ = socket.write_all(&response);
        });
        (format!("http://127.0.0.1:{port}"), request)
    }

    fn http_response(status: &str, headers: &[&str], body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n",
            body.len()
        );
        for header in headers {
            out.push_str(header);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        let mut out = out.into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// The request line as sent, the header lines in lowercase, and the body.
    fn split_request(request: &str) -> (Vec<String>, String) {
        let (head, body) = request.split_once("\r\n\r\n").unwrap_or((request, ""));
        let lines = head.lines().enumerate().map(|(index, line)| match index {
            0 => line.to_owned(),
            _ => line.to_lowercase(),
        });
        (lines.collect(), body.to_owned())
    }

    #[tokio::test]
    async fn replay_request는_모르는_요청_id를_거부한다() {
        let _scope = scope_guard();
        let context = ToolContext::for_tests();
        assert_eq!(
            call(&ReplayRequest, &context, json!({ "requestId": "nope" })).await,
            Err("unknown requestId: nope".to_owned())
        );
    }

    #[tokio::test]
    async fn replay_request는_캡처한_요청을_페이지_쿠키와_함께_다시_보낸다() {
        let _scope = scope_guard();
        let response = http_response(
            "201 Created",
            &[
                "Content-Type: application/json",
                "X-Dup: 1",
                "Set-Cookie: a=1",
                "x-dup: 2",
                "Set-Cookie: b=2",
            ],
            b"{\"ok\":true}",
        );
        let (origin, request) = serve_once(response);
        let (browser, context) = cookie_context(&[("sid", "abc"), ("theme", "dark")]);
        let url = format!("{origin}/api/items?x=1");
        let mut patch = capture(&context, "req-1", &url, "POST");
        patch.request_headers = Some(BTreeMap::from([
            ("Authorization".to_owned(), "Bearer t".to_owned()),
            ("Content-Type".to_owned(), "application/json".to_owned()),
            ("Cookie".to_owned(), "stale=1".to_owned()),
            // Matches the body: accepted, and written again by the client.
            ("content-length".to_owned(), "280".to_owned()),
        ]));
        patch.request_post_data = Some("{\"n\":1}".repeat(40));
        lock(&context.traffic).upsert(patch, 1);

        let text = call(&ReplayRequest, &context, json!({ "requestId": "req-1" }))
            .await
            .unwrap();
        let sent_body = "{\"n\":1}".repeat(40);
        assert_eq!(
            text,
            format!(
                "{{\n  \"status\": 201,\n  \"statusText\": \"Created\",\n  \"credentialsSent\": [\n    \"cookie\",\n    \"authorization\"\n  ],\n  \"sent\": {{\n    \"method\": \"POST\",\n    \"body\": {}\n  }},\n  \"headers\": {{\n    \"connection\": \"close\",\n    \"content-length\": \"11\",\n    \"content-type\": \"application/json\",\n    \"set-cookie\": \"b=2\",\n    \"x-dup\": \"1, 2\"\n  }},\n  \"body\": \"{{\\\"ok\\\":true}}\",\n  \"truncated\": false\n}}",
                // Only the first 200 characters of what was sent are echoed.
                json!(&sent_body[..200])
            )
        );

        // Cookies are asked for the origin, and replace the captured cookie.
        assert_eq!(*lock(&browser.asked), [origin.as_str()]);
        let (head, body) = split_request(&request.recv().unwrap());
        assert_eq!(head[0], "POST /api/items?x=1 HTTP/1.1");
        assert!(head.contains(&"cookie: sid=abc; theme=dark".to_owned()));
        assert!(head.contains(&"authorization: bearer t".to_owned()));
        assert!(head.contains(&"content-type: application/json".to_owned()));
        assert!(head.contains(&"content-length: 280".to_owned()));
        // What Node's fetch adds when the capture has none.
        assert!(head.contains(&"user-agent: node".to_owned()));
        assert!(head.contains(&"accept: */*".to_owned()));
        assert!(head.contains(&"accept-language: *".to_owned()));
        assert!(head.contains(&"sec-fetch-mode: cors".to_owned()));
        assert!(head.contains(&"accept-encoding: gzip, deflate".to_owned()));
        assert_eq!(body, sent_body);
    }

    #[tokio::test]
    async fn replay_request의_덮어쓰기는_캡처를_이기고_빈_헤더는_인증을_제거한다() {
        let _scope = scope_guard();
        let (origin, request) = serve_once(http_response("200 OK", &[], b"fine"));
        let (browser, context) = cookie_context(&[("sid", "abc")]);
        let mut patch = capture(&context, "req-1", "https://captured.test/old", "GET");
        patch.request_headers = Some(BTreeMap::from([
            ("authorization".to_owned(), "Bearer t".to_owned()),
            ("user-agent".to_owned(), "Mozilla/5.0".to_owned()),
            (
                "accept-encoding".to_owned(),
                "gzip, deflate, br, zstd".to_owned(),
            ),
        ]));
        lock(&context.traffic).upsert(patch, 1);

        let args = json!({
            "requestId": "req-1",
            "overrides": {
                "url": format!("{origin}/new"),
                "method": "put",
                "headers": { "Cookie": "", "Authorization": "", "X-Test": "1" },
                "body": "payload"
            }
        });
        let result = call_json(&ReplayRequest, &context, args).await;
        assert_eq!(result["status"], json!(200));
        assert_eq!(result["statusText"], json!("OK"));
        assert_eq!(result["credentialsSent"], json!([]));
        // The method is echoed as given; on the wire fetch upper-cases it.
        assert_eq!(
            result["sent"],
            json!({ "method": "put", "body": "payload" })
        );
        assert_eq!(result["body"], json!("fine"));

        // Cookies are looked up for the override URL, not the captured one.
        assert_eq!(*lock(&browser.asked), [origin]);
        let (head, body) = split_request(&request.recv().unwrap());
        // "put" was upper-cased on the way out.
        assert_eq!(head[0], "PUT /new HTTP/1.1");
        assert!(!head.iter().any(|line| line.starts_with("cookie:")));
        assert!(!head.iter().any(|line| line.starts_with("authorization:")));
        assert!(head.contains(&"x-test: 1".to_owned()));
        assert!(head.contains(&"user-agent: mozilla/5.0".to_owned()));
        assert!(head.contains(&"accept-encoding: gzip, deflate, br, zstd".to_owned()));
        // A string body without a content type is sent as text.
        assert!(head.contains(&"content-type: text/plain;charset=utf-8".to_owned()));
        assert_eq!(body, "payload");
    }

    #[tokio::test]
    async fn replay_request는_응답_본문을_10000자에서_자르고_잘렸다고_알린다() {
        let _scope = scope_guard();
        // 9,999 ASCII characters, then Hangul: the cut counts characters, not bytes.
        let long = format!("{}{}", "a".repeat(9_999), "가나다");
        let (origin, _request) = serve_once(http_response("200 OK", &[], long.as_bytes()));
        let context = ToolContext::for_tests();
        capture(&context, "req-1", &format!("{origin}/big"), "GET");
        let result = call_json(&ReplayRequest, &context, json!({ "requestId": "req-1" })).await;
        assert_eq!(result["body"], json!(format!("{}가", "a".repeat(9_999))));
        assert_eq!(result["truncated"], json!(true));
        // No browser cookies and no captured headers: nothing credential-like went out.
        assert_eq!(result["credentialsSent"], json!([]));
        assert_eq!(result["sent"], json!({ "method": "GET", "body": "" }));
    }

    #[tokio::test]
    async fn replay_request는_본문_없는_post에_content_length_0을_보낸다() {
        let _scope = scope_guard();
        // Node's fetch does; some servers answer 411 without it.
        let (origin, request) = serve_once(http_response("204 No Content", &[], b""));
        let context = ToolContext::for_tests();
        capture(&context, "req-1", &format!("{origin}/empty"), "post");
        let result = call_json(&ReplayRequest, &context, json!({ "requestId": "req-1" })).await;
        assert_eq!(result["status"], json!(204));
        assert_eq!(result["statusText"], json!("No Content"));
        assert_eq!(result["body"], json!(""));
        let (head, body) = split_request(&request.recv().unwrap());
        assert_eq!(head[0], "POST /empty HTTP/1.1");
        assert!(head.contains(&"content-length: 0".to_owned()), "{head:?}");
        assert!(!head.iter().any(|line| line.starts_with("content-type:")));
        assert_eq!(body, "");
    }

    #[tokio::test]
    async fn replay_request는_정확히_10000자인_본문은_자르지_않고_bom은_버린다() {
        let _scope = scope_guard();
        let exact = format!("\u{feff}{}", "b".repeat(10_000));
        let (origin, _request) = serve_once(http_response("404 Not Found", &[], exact.as_bytes()));
        let context = ToolContext::for_tests();
        capture(&context, "req-1", &format!("{origin}/exact"), "GET");
        let result = call_json(&ReplayRequest, &context, json!({ "requestId": "req-1" })).await;
        // An error status is still a result, not a tool error.
        assert_eq!(result["status"], json!(404));
        assert_eq!(result["statusText"], json!("Not Found"));
        assert_eq!(result["body"], json!("b".repeat(10_000)));
        assert_eq!(result["truncated"], json!(false));
    }

    #[tokio::test]
    async fn replay_request는_gzip_응답을_풀어서_돌려준다() {
        let _scope = scope_guard();
        // "hello gz" gzipped by Node's zlib.gzipSync.
        let gzipped: [u8; 28] = [
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x13, 0xcb, 0x48, 0xcd, 0xc9,
            0xc9, 0x57, 0x48, 0xaf, 0x02, 0x00, 0xbf, 0x96, 0x32, 0x5d, 0x08, 0x00, 0x00, 0x00,
        ];
        let response = http_response("200 OK", &["content-encoding: gzip"], &gzipped);
        let (origin, _request) = serve_once(response);
        let context = ToolContext::for_tests();
        capture(&context, "req-1", &format!("{origin}/gz"), "GET");
        let result = call_json(&ReplayRequest, &context, json!({ "requestId": "req-1" })).await;
        assert_eq!(result["body"], json!("hello gz"));
    }

    #[tokio::test]
    async fn replay_request는_범위_밖_주소로는_보내지_않는다() {
        let _scope = scope_guard();
        let (browser, context) = cookie_context(&[("sid", "abc")]);
        capture(&context, "req-1", "https://example.com/a", "GET");
        capture(&context, "req-2", "https://evil.test/a", "GET");
        set_scope(&["example.com"]);

        let blocked = "Out of scope: evil.test is not within the allowed scope [example.com]. Add it with scope_add if this target is intended.";
        // The override URL is what counts when there is one.
        let args =
            json!({ "requestId": "req-1", "overrides": { "url": "https://evil.test/steal" } });
        assert_eq!(
            call(&ReplayRequest, &context, args).await,
            Err(blocked.to_owned())
        );
        assert_eq!(
            call(&ReplayRequest, &context, json!({ "requestId": "req-2" })).await,
            Err(blocked.to_owned())
        );
        // Blocked before any cookie was read.
        assert!(lock(&browser.asked).is_empty());
        clear_scope();
    }

    #[tokio::test]
    async fn replay_request는_fetch가_거부하는_요청을_같은_문구로_거부한다() {
        let _scope = scope_guard();
        // Every message is what Node's fetch rejects with for the same input.
        async fn replay(url: &str, method: &str, overrides: Value) -> Result<String, String> {
            let context = ToolContext::for_tests();
            capture(&context, "req-1", url, method);
            let args = json!({ "requestId": "req-1", "overrides": overrides });
            call(&ReplayRequest, &context, args).await
        }
        let err = |text: &str| Err(text.to_owned());
        let url = "http://127.0.0.1:9/";

        assert_eq!(
            replay("not a url", "GET", json!({})).await,
            err("Failed to parse URL from not a url")
        );
        assert_eq!(
            replay("/x", "GET", json!({})).await,
            err("Failed to parse URL from /x")
        );
        assert_eq!(
            replay("http://u:p@127.0.0.1:9/", "A B", json!({})).await,
            err(
                "Request cannot be constructed from a URL that includes credentials: http://u:p@127.0.0.1:9/"
            )
        );
        assert_eq!(
            replay(url, "A B", json!({ "headers": { "a b": "x" } })).await,
            err("'A B' is not a valid HTTP method.")
        );
        assert_eq!(
            replay(url, "", json!({})).await,
            err("'' is not a valid HTTP method.")
        );
        assert_eq!(
            replay(url, "CONNECT", json!({})).await,
            err("'CONNECT' HTTP method is unsupported.")
        );
        assert_eq!(
            replay(url, "trace", json!({})).await,
            err("'trace' HTTP method is unsupported.")
        );
        assert_eq!(
            replay(url, "GET", json!({ "body": "x" })).await,
            err("Request with GET/HEAD method cannot have body.")
        );
        assert_eq!(
            replay(url, "head", json!({ "body": "x" })).await,
            err("Request with GET/HEAD method cannot have body.")
        );
        // A bad header is reported before the body check.
        assert_eq!(
            replay(
                url,
                "GET",
                json!({ "body": "x", "headers": { "a b": "x" } })
            )
            .await,
            err("Headers.append: \"a b\" is an invalid header name.")
        );
        assert_eq!(
            replay(url, "GET", json!({ "headers": { ":method": "GET" } })).await,
            err("Headers.append: \":method\" is an invalid header name.")
        );
        // Surrounding whitespace is trimmed before the value is checked.
        assert_eq!(
            replay(url, "GET", json!({ "headers": { "a": " \tx\u{0}y " } })).await,
            err("Headers.append: \"x\u{0}y\" is an invalid header value.")
        );
        assert_eq!(
            replay(url, "GET", json!({ "headers": { "a": "x\ny" } })).await,
            err("Headers.append: \"x\ny\" is an invalid header value.")
        );
        // Anything outside Latin-1 is refused before any header is looked at.
        assert_eq!(
            replay(url, "GET", json!({ "headers": { "a b": "x", "c": "x한" } })).await,
            err(
                "Cannot convert argument to a ByteString because the character at index 1 has a value of 54620 which is greater than 255."
            )
        );
        // A content-length that contradicts the body, an unknown scheme and a
        // refused connection all surface as fetch's one transport error.
        assert_eq!(
            replay(
                url,
                "POST",
                json!({ "body": "abc", "headers": { "content-length": "10" } })
            )
            .await,
            err("fetch failed")
        );
        assert_eq!(
            replay("ftp://a.test/x", "GET", json!({})).await,
            err("fetch failed")
        );
        assert_eq!(replay(url, "GET", json!({})).await, err("fetch failed"));
    }

    #[tokio::test]
    async fn replay_request의_overrides는_모르는_키를_거부한다() {
        let _scope = scope_guard();
        let context = ToolContext::for_tests();
        capture(&context, "req-1", "http://127.0.0.1:9/", "GET");
        let args = json!({ "requestId": "req-1", "overrides": { "header": { "a": "b" } } });
        let error = call(&ReplayRequest, &context, args).await.unwrap_err();
        assert!(error.starts_with(
            "MCP error -32602: Input validation error: Invalid arguments for tool replay_request: ["
        ));
        assert!(error.contains("Unrecognized key: \\\"header\\\""));
    }

    #[test]
    fn utf16_prefix는_utf16_단위로_세고_글자_중간에서는_자르지_않는다() {
        assert_eq!(utf16_prefix("abc", 5), "abc");
        assert_eq!(utf16_prefix("abc", 3), "abc");
        assert_eq!(utf16_prefix("abc", 2), "ab");
        assert_eq!(utf16_prefix("abc", 0), "");
        // Hangul is one unit each (three bytes); an emoji is two.
        assert_eq!(utf16_prefix("가나다", 2), "가나");
        assert_eq!(utf16_prefix("a😀b", 3), "a😀");
        assert_eq!(utf16_prefix("a😀b", 2), "a");
    }

    #[test]
    fn 대소문자만_다른_덮어쓰기_헤더는_나중에_준_것이_이긴다() {
        let folded = |headers: Value| {
            let headers = crate::js_key_order(headers);
            let overrides = fold_override_headers(headers.as_object().unwrap());
            merge_replay_headers(None, "sid=1", Some(&overrides))
        };
        // The blank one came last: the cookie is stripped, page cookie included.
        let stripped = folded(json!({ "cookie": "x=1", "Cookie": "" }));
        assert!(!stripped.contains_key("cookie"));
        let kept = folded(json!({ "Cookie": "", "cookie": "x=1" }));
        assert_eq!(kept.get("cookie").map(String::as_str), Some("x=1"));
        assert_eq!(
            folded(json!({ "X-A": "1", "x-a": "2", "X-a": "3" })),
            BTreeMap::from([
                ("cookie".to_owned(), "sid=1".to_owned()),
                ("x-a".to_owned(), "3".to_owned())
            ])
        );
    }

    #[tokio::test]
    async fn intercept_list는_정수_모양_헤더_이름을_자바스크립트_객체처럼_앞에_둔다() {
        // The TypeScript printed exactly this order for the same arguments (bun).
        let context = ToolContext::for_tests();
        let args = json!({
            "urlPattern": "*",
            "mode": "modify",
            "modifyHeaders": { "x-b": "2", "10": "a", "2": "b", "x-a": "1", "01": "c" }
        });
        call(&InterceptAdd, &context, args).await.unwrap();
        let listed = call(&InterceptList, &context, json!({})).await.unwrap();
        assert!(
            listed.ends_with(
                "\"modifyHeaders\": {\n      \"2\": \"b\",\n      \"10\": \"a\",\n      \"x-b\": \"2\",\n      \"x-a\": \"1\",\n      \"01\": \"c\"\n    }\n  }\n]"
            ),
            "{listed}"
        );
    }

    #[test]
    fn 규칙의_헤더는_준_순서대로_객체로_직렬화한다() {
        let with_headers = InterceptRule {
            modify_headers: Some(vec![
                ("x-b".to_owned(), "2".to_owned()),
                ("x-a".to_owned(), "1".to_owned()),
            ]),
            ..rule("r", "*", Mode::Block)
        };
        assert_eq!(
            serde_json::to_string(&with_headers).unwrap(),
            r#"{"id":"r","urlPattern":"*","mode":"block","stage":"Request","modifyHeaders":{"x-b":"2","x-a":"1"}}"#
        );
    }
}
