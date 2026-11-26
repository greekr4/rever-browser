//! Port of `src/shared/risky-tools.ts` — which permission requests must always prompt the user.
//!
//! A permission request is the ACP `RequestPermissionRequest` JSON, taken as a
//! `serde_json::Value` (the shape rever-agent's `to_permission_request` builds):
//! `{ sessionId, toolCall: { toolCallId, title?, rawInput? }, options: [{ kind, name, optionId }] }`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use url::Url;

// Tools whose side effects are destructive or high-volume enough that they must
// always prompt the user, regardless of the global auto-approve setting.
// These names mirror the MCP tool ids registered in src/main/mcp/tools/*.
pub const ALWAYS_RISKY_TOOLS: [&str; 6] = [
    "intruder_run",
    "burst_send",
    "payload_probe",
    "crlf_test",
    "path_probe",
    "lfi_probe",
];

// Replay-style tools are risky only for non-idempotent methods. A GET/HEAD
// replay is safe to auto-approve; a POST/PUT/DELETE replay on the user's live
// authenticated session is not.
pub const METHOD_GATED_TOOLS: [&str; 2] = ["replay_request", "repeater_send"];

fn is_safe_method(method: &str) -> bool {
    matches!(method.to_uppercase().as_str(), "GET" | "HEAD")
}

fn tool_call_of(req: &Value) -> Option<&Value> {
    req.get("toolCall")
}

fn raw_input_of(req: &Value) -> Option<&Value> {
    tool_call_of(req)?.get("rawInput")
}

/// JavaScript's `Number.prototype.toString`: plain digits from 1e-6 up to 1e21,
/// exponent form outside that. Rust and JavaScript both print the shortest
/// digits that round-trip, so only the layout needs matching.
fn js_number(x: f64) -> String {
    if x == 0.0 {
        return "0".to_owned(); // -0 prints as "0" too
    }
    if (1e-6..1e21).contains(&x.abs()) {
        return format!("{x}");
    }
    let text = format!("{x:e}");
    match text.split_once('e') {
        Some((mantissa, exponent)) if !exponent.starts_with('-') => {
            format!("{mantissa}e+{exponent}")
        }
        _ => text,
    }
}

/// JavaScript's `String(value)` for a parsed JSON value.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), js_number),
        Value::String(s) => s.clone(),
        // Array.prototype.join prints a null element as nothing.
        Value::Array(items) => items
            .iter()
            .map(|item| template_text(Some(item)))
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

/// What the TypeScript's `${field ?? ''}` prints. The field should be a string,
/// but the request is JSON from another process, so the other types are
/// rendered as JavaScript would render them.
fn template_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(other) => js_string(other),
    }
}

// claude-agent-acp exposes no dedicated tool-name field on the permission
// request, so we match against the toolCallId + human title (which for MCP
// tools carry the tool id, e.g. `mcp__rever-traffic__intruder_run`).
pub fn tool_haystack(req: &Value) -> String {
    let tc = tool_call_of(req);
    let field = |key: &str| template_text(tc.and_then(|tc| tc.get(key)));
    format!("{} {}", field("toolCallId"), field("title")).to_lowercase()
}

// Where a request field may live in a tool's input: top level, or inside the
// wrapper object each tool's schema uses (replay_request → overrides,
// repeater_send → modifications).
const INPUT_WRAPPERS: [&str; 3] = ["request", "overrides", "modifications"];

/// A key that is present counts even when its value is null, as in the
/// TypeScript (`!== undefined`): a top-level null hides the wrappers.
fn input_field<'a>(raw_input: &'a Value, key: &str) -> Option<&'a Value> {
    let o = raw_input.as_object()?;
    if let Some(v) = o.get(key) {
        return Some(v);
    }
    INPUT_WRAPPERS
        .iter()
        .find_map(|w| o.get(*w)?.as_object()?.get(key))
}

pub fn extract_method(raw_input: &Value) -> Option<&str> {
    input_field(raw_input, "method")?.as_str()
}

/// `new URL(raw).host`: the hostname, plus the port when it is not the scheme's default.
fn url_host(raw: &str) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    let host = url.host_str().unwrap_or("");
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

pub fn extract_target_host(raw_input: &Value) -> Option<String> {
    // path_probe/lfi_probe target a base URL under `baseUrl`, not `url`.
    let url = input_field(raw_input, "url")
        .filter(|v| !v.is_null())
        .or_else(|| input_field(raw_input, "baseUrl"));
    if let Some(url) = url.and_then(Value::as_str) {
        return url_host(url);
    }
    if let Some(host) = input_field(raw_input, "host").and_then(Value::as_str) {
        return Some(host.to_owned());
    }
    let cmd_url = COMMAND_URL_RE.find(shell_command_of(raw_input)?)?;
    url_host(cmd_url.as_str())
}

// Shell commands that send HTTP themselves (Bash → curl / wget / inline
// Python or Node). They skip the rever tools' scope check, approval prompt and
// audit trail, so any such command is gated under this pseudo tool id.
pub const SHELL_HTTP_TOOL: &str = "shell_http";

/// JavaScript's `\s`, as the inside of a character class. The regex crate's
/// `\s` differs by two characters: U+FEFF is whitespace only in JavaScript,
/// U+0085 only in Rust.
const JS_SPACE: &str = r"\t\n\x0B\x0C\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}";

/// The TypeScript's `(?:^|[;&|(`]|\$\()` under the `m` flag. The regex crate's
/// multi-line `^` only knows `\n`, so JavaScript's four line terminators are
/// spelled out; consuming one instead of looking behind it does not change
/// whether the pattern matches.
const COMMAND_POSITION: &str = r"(?:\A|[\n\r\u{2028}\u{2029};&|(`]|\$\()";

fn pattern(source: &str) -> Regex {
    Regex::new(source).expect("built-in pattern compiles")
}

// An HTTP client in command position: line start or after ; & | ( ` $( —
// so `grep curl src/` (curl as an argument) does not match.
// The TypeScript ends with the lookahead `(?=\s|$)`; without lookaround the
// same test is "a whitespace character or the end of the text follows".
static SHELL_HTTP_CLIENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    pattern(&format!(
        r"{COMMAND_POSITION}[{JS_SPACE}]*(?:sudo[{JS_SPACE}]+|env[{JS_SPACE}]+)?(?:curl|wget|xh|https?|ncat|nc)(?:[{JS_SPACE}]|\z)"
    ))
});

// `(?-u:\b)` is JavaScript's ASCII-only word boundary.
static INLINE_SCRIPT_HTTP_RE: LazyLock<Regex> = LazyLock::new(|| {
    pattern(&format!(
        r"{COMMAND_POSITION}[{JS_SPACE}]*(?:python3?|node|bun|deno)(?-u:\b)[^;&|]*(?-u:\b)(?:requests|urllib|httpx|aiohttp|fetch|axios|https?\.request)(?-u:\b)"
    ))
});

static COMMAND_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| pattern(&format!(r#"https?://[^{JS_SPACE}'"`)]+"#)));

pub fn shell_command_of(raw_input: &Value) -> Option<&str> {
    raw_input.as_object()?.get("command")?.as_str()
}

pub fn is_shell_http_command(command: &str) -> bool {
    SHELL_HTTP_CLIENT_RE.is_match(command) || INLINE_SCRIPT_HTTP_RE.is_match(command)
}

fn is_shell_http_request(req: &Value) -> bool {
    raw_input_of(req)
        .and_then(shell_command_of)
        .is_some_and(is_shell_http_command)
}

// Returns the matched risky tool id, or None when the request may be
// auto-approved. Method-gated tools return their id only for non-idempotent
// methods; an unknown method is treated as risky (fail safe).
pub fn matched_risky_tool(req: &Value) -> Option<&'static str> {
    if is_shell_http_request(req) {
        return Some(SHELL_HTTP_TOOL);
    }
    let hay = tool_haystack(req);
    if let Some(t) = ALWAYS_RISKY_TOOLS.into_iter().find(|t| hay.contains(t)) {
        return Some(t);
    }
    METHOD_GATED_TOOLS.into_iter().find(|t| {
        hay.contains(t)
            && !raw_input_of(req)
                .and_then(extract_method)
                .is_some_and(is_safe_method)
    })
}

/// The app reaches its own tools through `revb` in a shell, so a command that is
/// exactly one `revb` invocation counts as the app's own work. Anything else —
/// a pipe, a second command, a redirection — is a shell the user has to approve.
pub fn is_own_cli(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() {
        return false;
    }
    // Reject the metacharacters a second command hides behind.
    if command
        .chars()
        .any(|c| matches!(c, ';' | '|' | '&' | '`' | '<' | '>' | '(' | ')' | '$' | '\n'))
    {
        return false;
    }
    let Some(first) = command.split_whitespace().next() else {
        return false;
    };
    // `/opt/homebrew/bin/revb` and `./revb` are the same program.
    first.rsplit('/').next() == Some("revb")
}

pub fn is_risky_tool_request(req: &Value) -> bool {
    matched_risky_tool(req).is_some()
}

// Gated = any tool on either list, whatever the method. These must never be
// answered with allow_always: claude-agent-acp turns that into a session rule
// for the tool name, after which it stops asking — so one approval (or one
// auto-approved GET replay) would silently disable the prompt for every later
// call, POSTs included.
pub fn gated_tool_name(req: &Value) -> Option<&'static str> {
    if is_shell_http_request(req) {
        return Some(SHELL_HTTP_TOOL);
    }
    let hay = tool_haystack(req);
    ALWAYS_RISKY_TOOLS
        .into_iter()
        .chain(METHOD_GATED_TOOLS)
        .find(|t| hay.contains(t))
}

pub fn is_gated_tool_request(req: &Value) -> bool {
    gated_tool_name(req).is_some()
}

fn options(req: &Value) -> &[Value] {
    req.get("options")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn kind(option: &Value) -> &str {
    option.get("kind").and_then(Value::as_str).unwrap_or("")
}

fn option_id(option: &Value) -> Option<&str> {
    option.get("optionId")?.as_str()
}

fn option_of_kind<'a>(req: &'a Value, wanted: &str) -> Option<&'a str> {
    option_id(options(req).iter().find(|o| kind(o) == wanted)?)
}

/// The first option whose kind starts with `prefix`, else the first option, else "".
fn option_with_prefix(req: &Value, prefix: &str) -> String {
    let all = options(req);
    all.iter()
        .find(|o| kind(o).starts_with(prefix))
        .and_then(option_id)
        .or_else(|| all.first().and_then(option_id))
        .unwrap_or("")
        .to_owned()
}

pub fn find_reject_option(req: &Value) -> String {
    option_with_prefix(req, "reject")
}

// The allow option to use when approving: allow_once for gated tools,
// otherwise allow_always (so ordinary tools don't re-prompt every call).
pub fn choose_allow_option(req: &Value) -> String {
    let preferred = if is_gated_tool_request(req) {
        option_of_kind(req, "allow_once")
    } else {
        option_of_kind(req, "allow_always")
    };
    // An empty id is falsy in the TypeScript and falls through too.
    match preferred {
        Some(id) if !id.is_empty() => id.to_owned(),
        _ => option_with_prefix(req, "allow"),
    }
}

// Downgrades an explicit "Always Allow" pick on a gated tool to allow_once.
pub fn sanitize_selection(req: &Value, picked_id: &str) -> String {
    let picked_always = || {
        options(req)
            .iter()
            .find(|o| option_id(o) == Some(picked_id))
            .is_some_and(|o| kind(o) == "allow_always")
    };
    if !is_gated_tool_request(req) || !picked_always() {
        return picked_id.to_owned();
    }
    option_of_kind(req, "allow_once")
        .unwrap_or(picked_id)
        .to_owned()
}

// Main-process answer when the renderer prompt can't be reached: never let a
// risky action through unseen.
pub fn fallback_option(req: &Value) -> String {
    if is_risky_tool_request(req) {
        find_reject_option(req)
    } else {
        choose_allow_option(req)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // claude-agent-acp의 실제 옵션 세트 (acp-agent.js canUseTool)
    fn acp_options() -> Value {
        json!([
            { "kind": "allow_always", "name": "Always Allow", "optionId": "allow_always" },
            { "kind": "allow_once", "name": "Allow", "optionId": "allow" },
            { "kind": "reject_once", "name": "Reject", "optionId": "reject" }
        ])
    }

    fn req_with(tool_call_id: &str, title: &str, raw_input: Value) -> Value {
        json!({
            "sessionId": "s",
            "options": acp_options(),
            "toolCall": { "toolCallId": tool_call_id, "title": title, "rawInput": raw_input }
        })
    }

    /// The TypeScript helper's two-argument form: no rawInput at all.
    fn req(tool_call_id: &str, title: &str) -> Value {
        json!({
            "sessionId": "s",
            "options": acp_options(),
            "toolCall": { "toolCallId": tool_call_id, "title": title }
        })
    }

    // claude-agent-acp는 Bash 호출의 title을 명령어 자체로, rawInput을 {command}로 보낸다
    fn bash(command: &str) -> Value {
        req_with(
            "toolu_1",
            command,
            json!({ "command": command, "description": "d" }),
        )
    }

    #[test]
    fn 항상_위험한_툴은_메서드와_무관하게_매칭된다() {
        assert_eq!(
            matched_risky_tool(&req("mcp__rever-traffic__intruder_run", "Intruder")),
            Some("intruder_run")
        );
        assert_eq!(
            matched_risky_tool(&req("x", "Running burst_send")),
            Some("burst_send")
        );
    }

    #[test]
    fn 메서드_게이트_툴은_get_head면_안전으로_통과한다() {
        assert_eq!(
            matched_risky_tool(&req_with(
                "mcp__rever-traffic__replay_request",
                "t",
                json!({ "method": "GET" })
            )),
            None
        );
        assert_eq!(
            matched_risky_tool(&req_with("x", "repeater_send", json!({ "method": "head" }))),
            None
        );
    }

    #[test]
    fn 메서드_게이트_툴은_비멱등_메서드면_위험으로_매칭된다() {
        assert_eq!(
            matched_risky_tool(&req_with(
                "x",
                "replay_request",
                json!({ "method": "POST" })
            )),
            Some("replay_request")
        );
        assert_eq!(
            matched_risky_tool(&req_with(
                "x",
                "repeater_send",
                json!({ "request": { "method": "DELETE" } })
            )),
            Some("repeater_send")
        );
    }

    #[test]
    fn 실제_툴_스키마의_overrides_modifications_method를_읽는다() {
        assert_eq!(
            matched_risky_tool(&req_with(
                "x",
                "replay_request",
                json!({ "overrides": { "method": "GET" } })
            )),
            None
        );
        assert_eq!(
            matched_risky_tool(&req_with(
                "x",
                "replay_request",
                json!({ "overrides": { "method": "POST" } })
            )),
            Some("replay_request")
        );
        assert_eq!(
            matched_risky_tool(&req_with(
                "x",
                "repeater_send",
                json!({ "modifications": { "method": "get" } })
            )),
            None
        );
        assert_eq!(
            matched_risky_tool(&req_with(
                "x",
                "repeater_send",
                json!({ "modifications": { "method": "PUT" } })
            )),
            Some("repeater_send")
        );
    }

    #[test]
    fn 메서드를_알_수_없으면_안전하게_위험으로_취급한다() {
        assert_eq!(
            matched_risky_tool(&req_with("x", "replay_request", json!({}))),
            Some("replay_request")
        );
        assert_eq!(
            matched_risky_tool(&req("x", "replay_request")),
            Some("replay_request")
        );
    }

    #[test]
    fn 위험_목록에_없는_툴은_매칭되지_않는다() {
        assert_eq!(
            matched_risky_tool(&req("mcp__rever-traffic__list_requests", "List")),
            None
        );
        assert!(!is_risky_tool_request(&req("x", "browser_snapshot")));
    }

    #[test]
    fn 평면_중첩_method를_모두_읽는다() {
        assert_eq!(extract_method(&json!({ "method": "POST" })), Some("POST"));
        assert_eq!(
            extract_method(&json!({ "request": { "method": "PUT" } })),
            Some("PUT")
        );
        assert_eq!(extract_method(&json!({})), None);
        assert_eq!(extract_method(&Value::Null), None);
    }

    #[test]
    fn 메서드와_무관하게_게이트_대상_툴이면_true다() {
        assert!(is_gated_tool_request(&req_with(
            "x",
            "mcp__rever-traffic__repeater_send",
            json!({ "modifications": { "method": "GET" } })
        )));
        assert!(is_gated_tool_request(&req(
            "x",
            "mcp__rever-traffic__intruder_run"
        )));
        assert!(!is_gated_tool_request(&req(
            "x",
            "mcp__rever-traffic__list_requests"
        )));
    }

    #[test]
    fn curl_wget_실행은_메서드와_무관하게_위험이다() {
        assert_eq!(
            matched_risky_tool(&bash("curl -s -X POST http://localhost:8779/")),
            Some("shell_http")
        );
        assert_eq!(
            matched_risky_tool(&bash("curl https://example.com/")),
            Some("shell_http")
        );
        assert_eq!(
            matched_risky_tool(&bash("wget -qO- https://example.com")),
            Some("shell_http")
        );
    }

    #[test]
    fn 루프_서브셸_파이프_안의_curl도_잡는다() {
        assert_eq!(
            matched_risky_tool(&bash(
                "for i in $(seq 1 3); do c=$(curl -s http://x.io/); done"
            )),
            Some("shell_http")
        );
        assert_eq!(
            matched_risky_tool(&bash("echo hi && curl http://x.io")),
            Some("shell_http")
        );
    }

    #[test]
    fn 인라인_python_node_http_스크립트도_잡는다() {
        assert_eq!(
            matched_risky_tool(&bash(
                "python3 -c \"import requests; requests.post('http://x.io')\""
            )),
            Some("shell_http")
        );
        assert_eq!(
            matched_risky_tool(&bash("node -e \"fetch('http://x.io',{method:'POST'})\"")),
            Some("shell_http")
        );
    }

    #[test]
    fn curl이_인자로만_등장하는_명령은_위험이_아니다() {
        assert_eq!(matched_risky_tool(&bash("grep -rn curl src/")), None);
        assert_eq!(matched_risky_tool(&bash("git status")), None);
        assert_eq!(matched_risky_tool(&bash("cat curl-notes.md")), None);
    }

    #[test]
    fn 게이트_대상이라_allow_once_세션_허용_키를_쓰고_대상_host를_보여준다() {
        let r = bash("curl -X POST http://localhost:8779/api");
        assert_eq!(gated_tool_name(&r), Some("shell_http"));
        assert_eq!(choose_allow_option(&r), "allow");
        assert_eq!(
            extract_target_host(&r["toolCall"]["rawInput"]),
            Some("localhost:8779".to_owned())
        );
    }

    #[test]
    fn 메서드와_무관하게_게이트_툴_이름을_돌려준다() {
        assert_eq!(
            gated_tool_name(&req_with(
                "x",
                "mcp__rever-traffic__repeater_send",
                json!({ "modifications": { "method": "GET" } })
            )),
            Some("repeater_send")
        );
        assert_eq!(
            gated_tool_name(&req("x", "mcp__rever-traffic__lfi_probe")),
            Some("lfi_probe")
        );
        assert_eq!(
            gated_tool_name(&req("x", "mcp__rever-traffic__list_requests")),
            None
        );
    }

    #[test]
    fn 게이트_대상_툴은_세션_규칙이_남지_않도록_allow_once를_고른다() {
        assert_eq!(choose_allow_option(&req("x", "intruder_run")), "allow");
        // GET 자동승인이 allow_always면 이후 POST까지 세션 전체가 허용돼 버린다
        assert_eq!(
            choose_allow_option(&req_with(
                "x",
                "repeater_send",
                json!({ "modifications": { "method": "GET" } })
            )),
            "allow"
        );
    }

    #[test]
    fn 일반_툴은_기존처럼_allow_always를_고른다() {
        assert_eq!(
            choose_allow_option(&req("x", "list_requests")),
            "allow_always"
        );
    }

    #[test]
    fn 게이트_대상_툴에서_고른_allow_always를_allow_once로_낮춘다() {
        assert_eq!(
            sanitize_selection(&req("x", "burst_send"), "allow_always"),
            "allow"
        );
    }

    #[test]
    fn 그_외_선택은_그대로_둔다() {
        assert_eq!(
            sanitize_selection(&req("x", "burst_send"), "reject"),
            "reject"
        );
        assert_eq!(
            sanitize_selection(&req("x", "list_requests"), "allow_always"),
            "allow_always"
        );
    }

    #[test]
    fn ui_왕복이_실패하면_위험_툴은_거부한다() {
        assert_eq!(fallback_option(&req("x", "lfi_probe")), "reject");
        assert_eq!(
            fallback_option(&req_with(
                "x",
                "replay_request",
                json!({ "overrides": { "method": "DELETE" } })
            )),
            "reject"
        );
    }

    #[test]
    fn 위험하지_않은_요청은_허용하되_게이트_툴은_allow_once로_남긴다() {
        assert_eq!(fallback_option(&req("x", "list_requests")), "allow_always");
        assert_eq!(
            fallback_option(&req_with(
                "x",
                "replay_request",
                json!({ "overrides": { "method": "GET" } })
            )),
            "allow"
        );
    }

    #[test]
    fn url에서_host를_뽑는다() {
        assert_eq!(
            extract_target_host(&json!({ "url": "https://api.example.com/v3/order" })),
            Some("api.example.com".to_owned())
        );
        assert_eq!(
            extract_target_host(&json!({ "request": { "url": "https://x.io:8443/a" } })),
            Some("x.io:8443".to_owned())
        );
    }

    #[test]
    fn overrides_modifications_url에서도_host를_뽑는다() {
        assert_eq!(
            extract_target_host(&json!({ "overrides": { "url": "https://evil.test/x" } })),
            Some("evil.test".to_owned())
        );
        assert_eq!(
            extract_target_host(&json!({ "modifications": { "url": "http://127.0.0.1:8777/a" } })),
            Some("127.0.0.1:8777".to_owned())
        );
    }

    #[test]
    fn host_필드나_잘못된_url도_처리한다() {
        assert_eq!(
            extract_target_host(&json!({ "host": "cdn.example.com" })),
            Some("cdn.example.com".to_owned())
        );
        assert_eq!(extract_target_host(&json!({ "url": "not a url" })), None);
        assert_eq!(extract_target_host(&json!(42)), None);
    }

    // --- Not in the TypeScript tests: JavaScript behavior the port has to keep. ---

    #[test]
    fn base_url과_기본_포트와_null_url을_자바스크립트처럼_다룬다() {
        assert_eq!(
            extract_target_host(&json!({ "baseUrl": "http://files.test:8080/static" })),
            Some("files.test:8080".to_owned())
        );
        // `??` skips a null url, but a non-string url stops the url lookup.
        assert_eq!(
            extract_target_host(&json!({ "url": null, "baseUrl": "https://b.test/" })),
            Some("b.test".to_owned())
        );
        assert_eq!(
            extract_target_host(
                &json!({ "url": 5, "baseUrl": "https://b.test/", "host": "h.test" })
            ),
            Some("h.test".to_owned())
        );
        // URL.host drops the scheme's default port and brackets IPv6.
        assert_eq!(
            extract_target_host(&json!({ "url": "https://a.test:443/x" })),
            Some("a.test".to_owned())
        );
        assert_eq!(
            extract_target_host(&json!({ "url": "http://[::1]:9000/x" })),
            Some("[::1]:9000".to_owned())
        );
        // A URL with no authority has an empty host, not a missing one.
        assert_eq!(
            extract_target_host(&json!({ "url": "mailto:a@b.test" })),
            Some(String::new())
        );
        // A bad url wins over a usable host field.
        assert_eq!(
            extract_target_host(&json!({ "url": "nope", "host": "h.test" })),
            None
        );
    }

    #[test]
    fn 명령어_속_url은_따옴표와_괄호에서_끊긴다() {
        assert_eq!(
            extract_target_host(&json!({ "command": "c=$(curl -s 'https://q.test:8443/a b')" })),
            Some("q.test:8443".to_owned())
        );
        assert_eq!(
            extract_target_host(&json!({ "command": "echo $(curl http://p.test/)" })),
            Some("p.test".to_owned())
        );
        assert_eq!(extract_target_host(&json!({ "command": "ls -la" })), None);
        assert_eq!(extract_target_host(&json!({ "command": 7 })), None);
    }

    #[test]
    fn 최상위_null_method는_래퍼를_가리고_배열_입력은_무시한다() {
        assert_eq!(
            extract_method(&json!({ "method": null, "request": { "method": "GET" } })),
            None
        );
        assert_eq!(extract_method(&json!({ "method": 1 })), None);
        assert_eq!(extract_method(&json!(["method"])), None);
        assert_eq!(extract_method(&json!({ "request": ["method"] })), None);
        assert_eq!(extract_method(&json!("method")), None);
    }

    #[test]
    fn 셸_http_판정은_명령_위치와_줄_경계를_본다() {
        for command in [
            "curl",
            "sudo curl -I https://a.test",
            "env  wget x",
            "ls; nc -l 80",
            "a | ncat b 1",
            "echo `http :8080`",
            "(https example.test)",
            "echo ok\ncurl http://a.test",
            "echo ok\r\n  xh get a.test",
            "echo ok\u{2028}curl a.test",
            "curl\u{FEFF}-s a.test",
            "node script.js && bun -e 'await fetch(u)'",
            "deno eval \"axios.get(u)\"",
            "python -c 'import urllib.request'",
            "node -e \"https.request(u)\"",
            "a\rcurl x",
            "curl\u{2029}",
            // `\b` is ASCII-only in JavaScript: "é" is not a word character.
            "node\u{e9} fetch",
            "node -e '\u{e9}fetch'",
        ] {
            assert!(is_shell_http_command(command), "{command:?}");
        }
        for command in [
            "",
            "curling",
            "echo curl",
            "mycurl http://a.test",
            "curl-config --version",
            "sudo -u root curl a.test",
            "nc\u{85}-l",
            "node build.js",
            "python3 fetcher.py",
            "node -e 'x()' ; echo fetch",
            "pythonx -c 'requests.get(u)'",
            "nodefetch",
            "node -e \"require('https').request(u)\"",
        ] {
            assert!(!is_shell_http_command(command), "{command:?}");
        }
    }

    #[test]
    fn 건초더미는_소문자이고_빠진_필드는_빈_문자열이다() {
        assert_eq!(
            tool_haystack(&req("MCP__X__Intruder_Run", "Title")),
            "mcp__x__intruder_run title"
        );
        assert_eq!(
            tool_haystack(&json!({ "toolCall": { "title": "T" } })),
            " t"
        );
        assert_eq!(tool_haystack(&json!({})), " ");
        assert_eq!(tool_haystack(&json!({ "toolCall": null })), " ");
        assert_eq!(
            matched_risky_tool(&req("x", "Running BURST_SEND now")),
            Some("burst_send")
        );
    }

    #[test]
    fn 문자열이_아닌_title도_위험_툴_이름을_숨기지_못한다() {
        let r = json!({
            "options": acp_options(),
            "toolCall": { "toolCallId": 7, "title": ["intruder_run"] }
        });
        assert_eq!(matched_risky_tool(&r), Some("intruder_run"));
        assert_eq!(fallback_option(&r), "reject");
    }

    // Expected values are what the TypeScript prints for the same JSON (run with bun).
    #[test]
    fn 문자열이_아닌_필드는_자바스크립트의_문자열_변환대로_건초더미에_들어간다() {
        let hay = |tool_call: Value| tool_haystack(&json!({ "toolCall": tool_call }));
        assert_eq!(
            hay(json!({ "toolCallId": 7, "title": ["intruder_run"] })),
            "7 intruder_run"
        );
        assert_eq!(
            hay(json!({ "toolCallId": 1.5, "title": [1, [2, "burst_send"], null] })),
            "1.5 1,2,burst_send,"
        );
        assert_eq!(
            hay(json!({ "toolCallId": [null, "a"], "title": [] })),
            ",a "
        );
        assert_eq!(hay(json!({ "toolCallId": null, "title": false })), " false");
        assert_eq!(hay(json!({ "toolCallId": 1e21, "title": 0 })), "1e+21 0");
        assert_eq!(
            hay(json!({ "toolCallId": -0.0, "title": 123456789012345680000.0 })),
            "0 123456789012345680000"
        );
        assert_eq!(
            hay(json!({ "toolCallId": 0.000001, "title": 1e-7 })),
            "0.000001 1e-7"
        );
        // An object prints as "[object Object]", so a tool id inside it is not seen.
        let r = json!({
            "options": acp_options(),
            "toolCall": { "toolCallId": true, "title": { "a": "intruder_run" } }
        });
        assert_eq!(tool_haystack(&r), "true [object object]");
        assert_eq!(matched_risky_tool(&r), None);
        assert_eq!(gated_tool_name(&r), None);
        assert_eq!(fallback_option(&r), "allow_always");
    }

    #[test]
    fn 옵션이_없거나_깨져_있어도_패닉_없이_빈_문자열이나_첫_옵션을_돌려준다() {
        for broken in [
            json!(null),
            json!("x"),
            json!({}),
            json!({ "options": null }),
            json!({ "options": [] }),
            json!({ "options": [null, 3, {}] }),
        ] {
            assert_eq!(find_reject_option(&broken), "", "{broken}");
            assert_eq!(choose_allow_option(&broken), "", "{broken}");
            assert_eq!(fallback_option(&broken), "", "{broken}");
            assert_eq!(sanitize_selection(&broken, "pick"), "pick", "{broken}");
        }
        // No reject/allow kind: the first option stands in.
        let only = json!({ "options": [{ "kind": "other", "optionId": "first" }] });
        assert_eq!(find_reject_option(&only), "first");
        assert_eq!(choose_allow_option(&only), "first");
    }

    #[test]
    fn 선호_옵션_id가_비어_있으면_allow로_시작하는_다른_옵션을_고른다() {
        let r = json!({
            "toolCall": { "toolCallId": "x", "title": "list_requests" },
            "options": [
                { "kind": "allow_always", "optionId": "" },
                { "kind": "allow_once", "optionId": "once" }
            ]
        });
        // The first allow* option is the empty one, so the first option's id wins: "".
        assert_eq!(choose_allow_option(&r), "");
        let r = json!({
            "toolCall": { "toolCallId": "x", "title": "intruder_run" },
            "options": [
                { "kind": "allow_always", "optionId": "always" },
                { "kind": "reject_once", "optionId": "no" }
            ]
        });
        // Gated, no allow_once: falls back to the first allow* option, and an
        // allow_always pick has nothing to be downgraded to.
        assert_eq!(choose_allow_option(&r), "always");
        assert_eq!(sanitize_selection(&r, "always"), "always");
        assert_eq!(sanitize_selection(&r, "unknown"), "unknown");
    }

    #[test]
    fn revb_한_번짜리_명령만_앱_자신의_일로_본다() {
        assert!(is_own_cli("revb list"));
        assert!(is_own_cli("  revb call list_requests '{\"limit\":5}'  "));
        assert!(is_own_cli("/opt/homebrew/bin/revb scan http://127.0.0.1:8777"));
        // A second command, a pipe or a redirection is the user's call.
        assert!(!is_own_cli("revb list; rm -rf ~"));
        assert!(!is_own_cli("revb list && curl http://x"));
        assert!(!is_own_cli("revb list | sh"));
        assert!(!is_own_cli("revb list > /tmp/x"));
        assert!(!is_own_cli("revb list $(whoami)"));
        assert!(!is_own_cli("echo revb"));
        assert!(!is_own_cli(""));
        assert!(!is_own_cli("revbx list"));
    }
}
