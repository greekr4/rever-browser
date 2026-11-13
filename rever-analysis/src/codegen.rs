//! Port of `src/main/mcp/codegen.ts` — turns a captured request into runnable
//! python / curl / typescript, with credentials masked by default.

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use rever_core::secret_scan::mask;
use rever_core::traffic::Headers;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::{
    array_index, decode_uri_component, encode_uri_component, js_trim, json_parse, json_string,
    json_stringify,
};

/// A string record in the order a JavaScript object would enumerate it.
pub type Record = IndexMap<String, String>;

// Neutral intermediate form for a captured request, shared by every language
// emitter (python / curl / typescript) and reused by recipe/openapi codegen.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyType {
    Json,
    Form,
    Text,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestIR {
    pub method: String,
    pub url: String,
    pub headers: Record,
    pub cookies: Record,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub body_type: BodyType,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CodegenInput<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub request_headers: Option<&'a Headers>,
    pub request_post_data: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodegenLang {
    Python,
    Curl,
    Typescript,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PythonLibrary {
    #[default]
    Requests,
    Httpx,
}

#[derive(Clone, Copy, Debug)]
pub struct CodegenOptions {
    pub lang: CodegenLang,
    /// Default true.
    pub mask_secrets: Option<bool>,
    /// Default requests.
    pub python_library: Option<PythonLibrary>,
}

// Header names whose values are credentials and should be masked by default.
// The `i` flag folds ASCII letters only, as it does in JavaScript for these.
static SENSITIVE_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        "^(?i-u:authorization|cookie|proxy-authorization|x-api-key|api-key|x-auth-token|x-access-token|x-csrf-token|x-xsrf-token)$",
    )
    .expect("the pattern is a constant")
});
// Cookie / query-param / body-field names that look like credentials or
// session material.
static SENSITIVE_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        "(?i-u:token|session|sid|auth|secret|key|jwt|csrf|xsrf|pass(word|wd)?|pwd|credential)",
    )
    .expect("the pattern is a constant")
});

/// Sets `rec[key] = value` the way a JavaScript object keeps its keys: array
/// indices first in numeric order, the rest in insertion order.
fn set(rec: &mut Record, key: String, value: String) {
    let Some(index) = array_index(&key).filter(|_| !rec.contains_key(&key)) else {
        rec.insert(key, value);
        return;
    };
    let at = rec
        .keys()
        .position(|k| array_index(k).is_none_or(|other| other > index))
        .unwrap_or(rec.len());
    rec.shift_insert(at, key, value);
}

fn header_value<'a>(headers: &'a Record, name: &str) -> Option<&'a str> {
    let lc = name.to_lowercase();
    headers
        .iter()
        .find(|(k, _)| k.to_lowercase() == lc)
        .map(|(_, v)| v.as_str())
}

fn detect_body_type(headers: &Record, body: Option<&str>) -> BodyType {
    if body.is_none() {
        return BodyType::None;
    }
    let ct = header_value(headers, "content-type")
        .unwrap_or("")
        .to_lowercase();
    if ct.contains("json") {
        return BodyType::Json;
    }
    if ct.contains("x-www-form-urlencoded") {
        return BodyType::Form;
    }
    BodyType::Text
}

pub fn build_request_ir(input: &CodegenInput) -> RequestIR {
    let mut headers = Record::new();
    let mut cookie_header = "";
    for (k, v) in input.request_headers.into_iter().flatten() {
        if k.to_lowercase() == "cookie" {
            cookie_header = v;
            continue;
        }
        set(&mut headers, k.clone(), v.clone());
    }
    let mut cookies = Record::new();
    for part in cookie_header.split(';') {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        set(
            &mut cookies,
            js_trim(name).to_owned(),
            js_trim(value).to_owned(),
        );
    }
    let method = if input.method.is_empty() {
        "GET"
    } else {
        input.method
    };
    RequestIR {
        method: method.to_uppercase(),
        url: input.url.to_owned(),
        body_type: detect_body_type(&headers, input.request_post_data),
        headers,
        cookies,
        body: input.request_post_data.map(str::to_owned),
    }
}

fn mask_record(rec: &Record, re: &Regex) -> Record {
    rec.iter()
        .map(|(k, v)| {
            let value = if re.is_match(k) { mask(v) } else { v.clone() };
            (k.clone(), value)
        })
        .collect()
}

fn mask_url(url: &str) -> String {
    let Ok(mut u) = Url::parse(url) else {
        return url.to_owned();
    };
    let original: Vec<(String, String)> = u.query_pairs().into_owned().collect();
    let mut params = original.clone();
    let mut changed = false;
    for (k, v) in &original {
        if !SENSITIVE_FIELD_RE.is_match(k) {
            continue;
        }
        // URLSearchParams.set: the first pair of that name takes the value,
        // later pairs of the same name go.
        let mut first = true;
        params.retain_mut(|(name, value)| {
            if name != k {
                return true;
            }
            if first {
                *value = mask(v);
            }
            std::mem::take(&mut first)
        });
        changed = true;
    }
    if !changed {
        return url.to_owned();
    }
    u.query_pairs_mut().clear().extend_pairs(params);
    u.into()
}

/// The value with sensitive string fields masked, and whether any was.
fn mask_json_value(v: Value) -> (Value, bool) {
    match v {
        Value::Array(items) => {
            let mut changed = false;
            let items = items
                .into_iter()
                .map(|x| {
                    let (value, c) = mask_json_value(x);
                    changed |= c;
                    value
                })
                .collect();
            (Value::Array(items), changed)
        }
        Value::Object(map) => {
            let mut changed = false;
            let map = map
                .into_iter()
                .map(|(k, x)| {
                    let (value, c) = match x {
                        Value::String(s) if SENSITIVE_FIELD_RE.is_match(&k) => {
                            (Value::String(mask(&s)), true)
                        }
                        other => mask_json_value(other),
                    };
                    changed |= c;
                    (k, value)
                })
                .collect();
            (Value::Object(map), changed)
        }
        other => (other, false),
    }
}

fn mask_body(ir: &RequestIR) -> Option<String> {
    let body = ir.body.as_deref()?;
    let masked = match ir.body_type {
        BodyType::Json => json_parse(body).and_then(|parsed| {
            let (value, changed) = mask_json_value(parsed);
            // Re-serialize only when something was masked, so untouched bodies keep
            // their exact bytes.
            changed.then(|| json_stringify(&value))
        }),
        BodyType::Form => {
            let fields = parse_form_body(body);
            fields
                .keys()
                .any(|k| SENSITIVE_FIELD_RE.is_match(k))
                .then(|| {
                    mask_record(&fields, &SENSITIVE_FIELD_RE)
                        .iter()
                        .map(|(k, v)| {
                            format!("{}={}", encode_uri_component(k), encode_uri_component(v))
                        })
                        .collect::<Vec<_>>()
                        .join("&")
                })
        }
        BodyType::Text | BodyType::None => None,
    };
    Some(masked.unwrap_or_else(|| body.to_owned()))
}

// Credential-bearing parts of the request (headers, cookies, query params,
// JSON/form body fields) with their values masked. Plain-text bodies are left
// as-is — there's no structure to find a secret in.
fn apply_mask(ir: &RequestIR, on: bool) -> RequestIR {
    if !on {
        return ir.clone();
    }
    RequestIR {
        method: ir.method.clone(),
        url: mask_url(&ir.url),
        headers: mask_record(&ir.headers, &SENSITIVE_HEADER_RE),
        cookies: mask_record(&ir.cookies, &SENSITIVE_FIELD_RE),
        body: mask_body(ir),
        body_type: ir.body_type,
    }
}

fn parse_form_body(body: &str) -> Record {
    let mut out = Record::new();
    for part in body.split('&') {
        if part.is_empty() {
            continue;
        }
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        // Both or neither: one undecodable half keeps the pair raw.
        let (k, v) = match (decode_uri_component(k), decode_uri_component(v)) {
            (Some(k), Some(v)) => (k, v),
            _ => (k.to_owned(), v.to_owned()),
        };
        set(&mut out, k, v);
    }
    out
}

/// `JSON.stringify(v, null, indent)` of a string record.
fn pretty(v: &Record, indent: &str) -> String {
    if v.is_empty() {
        return "{}".into();
    }
    let lines: Vec<String> = v
        .iter()
        .map(|(k, v)| format!("{indent}{}: {}", json_string(k), json_string(v)))
        .collect();
    format!("{{\n{}\n}}", lines.join(",\n"))
}

// A JSON object of string values is already a valid Python dict literal.
fn py_dict(v: &Record) -> String {
    pretty(v, "    ")
}

// POSIX single-quoting: nothing inside is expanded, so captured values like
// `$(…)` or backticks stay literal when the user runs the command.
fn shq(v: &str) -> String {
    format!("'{}'", v.replace('\'', r"'\''"))
}

fn cookie_string(cookies: &Record) -> String {
    cookies
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn emit_python(raw: &RequestIR, mask_secrets: bool, library: PythonLibrary) -> String {
    let (lib, client_class) = match library {
        PythonLibrary::Httpx => ("httpx", "httpx.Client"),
        PythonLibrary::Requests => ("requests", "requests.Session"),
    };
    let ir = apply_mask(raw, mask_secrets);
    let method = ir.method.to_lowercase();
    let mut lines = vec![
        format!("import {lib}"),
        String::new(),
        format!("s = {client_class}()"),
    ];
    if !ir.headers.is_empty() {
        lines.push(format!("s.headers.update({})", py_dict(&ir.headers)));
    }
    if !ir.cookies.is_empty() {
        lines.push(format!("s.cookies.update({})", py_dict(&ir.cookies)));
    }

    let mut call = format!("resp = s.{method}({}", json_string(&ir.url));
    if let Some(body) = &ir.body {
        let data = match ir.body_type {
            BodyType::Form => py_dict(&parse_form_body(body)),
            // Send the exact JSON bytes; Content-Type comes from the captured headers.
            _ => json_string(body),
        };
        lines.push(format!("data = {data}"));
        call.push_str(", data=data");
    }
    call.push(')');
    lines.push(call);
    lines.push("print(resp.status_code, resp.text[:500])".into());
    lines.join("\n")
}

pub fn emit_curl(raw: &RequestIR, mask_secrets: bool) -> String {
    let ir = apply_mask(raw, mask_secrets);
    let plain = !ir.method.is_empty() && ir.method.bytes().all(|b| b.is_ascii_uppercase());
    let method = if plain {
        ir.method.clone()
    } else {
        shq(&ir.method)
    };
    let mut parts = vec![format!("curl -X {method} {}", shq(&ir.url))];
    for (k, v) in &ir.headers {
        parts.push(format!("  -H {}", shq(&format!("{k}: {v}"))));
    }
    let cookie_str = cookie_string(&ir.cookies);
    if !cookie_str.is_empty() {
        parts.push(format!("  -b {}", shq(&cookie_str)));
    }
    // --data-raw: plain --data would read a file when the body starts with "@".
    if let Some(body) = &ir.body {
        parts.push(format!("  --data-raw {}", shq(body)));
    }
    parts.join(" \\\n")
}

pub fn emit_typescript(raw: &RequestIR, mask_secrets: bool) -> String {
    let ir = apply_mask(raw, mask_secrets);
    let mut headers = ir.headers.clone();
    let cookie_str = cookie_string(&ir.cookies);
    if !cookie_str.is_empty() {
        headers.insert("Cookie".into(), cookie_str);
    }
    let mut lines = vec![
        format!("const res = await fetch({}, {{", json_string(&ir.url)),
        format!("  method: {},", json_string(&ir.method)),
        format!(
            "  headers: {},",
            pretty(&headers, "  ").replace('\n', "\n  ")
        ),
        format!(
            "  credentials: 'include'{}",
            if ir.body.is_some() { "," } else { "" }
        ),
    ];
    if let Some(body) = &ir.body {
        lines.push(format!("  body: {}", json_string(body)));
    }
    lines.push("})".into());
    lines.push("console.log(res.status, await res.text())".into());
    lines.join("\n")
}

pub fn generate_code(input: &CodegenInput, opts: &CodegenOptions) -> String {
    let ir = build_request_ir(input);
    let mask_secrets = opts.mask_secrets.unwrap_or(true);
    match opts.lang {
        CodegenLang::Curl => emit_curl(&ir, mask_secrets),
        CodegenLang::Typescript => emit_typescript(&ir, mask_secrets),
        CodegenLang::Python => {
            emit_python(&ir, mask_secrets, opts.python_library.unwrap_or_default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Headers {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn json_post_headers() -> Headers {
        headers(&[
            ("Content-Type", "application/json"),
            ("Authorization", "Bearer supersecrettoken1234567890"),
            ("Cookie", "SESSIONID=abcdef1234567890; theme=dark"),
        ])
    }

    fn json_post(headers: &Headers) -> CodegenInput<'_> {
        CodegenInput {
            method: "post",
            url: "https://api.example.com/v3/order",
            request_headers: Some(headers),
            request_post_data: Some(r#"{"item":"x","qty":2}"#),
        }
    }

    fn opts(lang: CodegenLang) -> CodegenOptions {
        CodegenOptions {
            lang,
            mask_secrets: None,
            python_library: None,
        }
    }

    fn raw(lang: CodegenLang) -> CodegenOptions {
        CodegenOptions {
            mask_secrets: Some(false),
            ..opts(lang)
        }
    }

    const JSON: &[(&str, &str)] = &[("content-type", "application/json")];
    const FORM: &[(&str, &str)] = &[("content-type", "application/x-www-form-urlencoded")];

    fn post<'a>(url: &'a str, headers: &'a Headers, body: &'a str) -> CodegenInput<'a> {
        CodegenInput {
            method: "POST",
            url,
            request_headers: Some(headers),
            request_post_data: Some(body),
        }
    }

    // ── buildRequestIR ──────────────────────────────────────────────────────

    #[test]
    fn 쿠키_헤더를_분리하고_메서드를_대문자화한다() {
        let h = json_post_headers();
        let ir = build_request_ir(&json_post(&h));
        assert_eq!(ir.method, "POST");
        assert_eq!(ir.headers.get("Cookie"), None);
        assert_eq!(
            ir.cookies.get("SESSIONID").map(String::as_str),
            Some("abcdef1234567890")
        );
        assert_eq!(ir.cookies.get("theme").map(String::as_str), Some("dark"));
    }

    #[test]
    fn content_type으로_body_type을_판별한다() {
        let h = json_post_headers();
        assert_eq!(build_request_ir(&json_post(&h)).body_type, BodyType::Json);
        let form = headers(FORM);
        assert_eq!(
            build_request_ir(&post("u", &form, "a=1&b=2")).body_type,
            BodyType::Form
        );
    }

    // ── generateCode masking ────────────────────────────────────────────────

    #[test]
    fn 기본값으로_민감_헤더_쿠키를_마스킹한다() {
        let h = json_post_headers();
        let py = generate_code(&json_post(&h), &opts(CodegenLang::Python));
        assert!(!py.contains("supersecrettoken1234567890"));
        assert!(!py.contains("abcdef1234567890"));
        assert!(py.contains("Bear…7890")); // masked authorization value
    }

    #[test]
    fn mask_secrets가_false면_원문을_그대로_낸다() {
        let h = json_post_headers();
        let py = generate_code(&json_post(&h), &raw(CodegenLang::Python));
        assert!(py.contains("supersecrettoken1234567890"));
    }

    #[test]
    fn url_쿼리의_토큰_파라미터를_마스킹한다() {
        let c = generate_code(
            &CodegenInput {
                method: "GET",
                url: "https://x.io/a?access_token=querysecret123456789&page=2",
                ..CodegenInput::default()
            },
            &opts(CodegenLang::Curl),
        );
        assert!(!c.contains("querysecret123456789"));
        assert!(c.contains("page=2"));
    }

    #[test]
    fn json_바디의_비밀번호_토큰_필드를_마스킹한다() {
        let h = headers(JSON);
        let py = generate_code(
            &post(
                "https://x.io/login",
                &h,
                r#"{"user":"me","password":"hunter2hunter2","nested":{"refresh_token":"rt-0123456789abcdef"}}"#,
            ),
            &opts(CodegenLang::Python),
        );
        assert!(!py.contains("hunter2hunter2"));
        assert!(!py.contains("rt-0123456789abcdef"));
        assert!(py.contains("me"));
    }

    #[test]
    fn form_바디의_민감_필드를_마스킹한다() {
        let h = headers(FORM);
        let c = generate_code(
            &post("https://x.io/login", &h, "user=me&passwd=formsecret999"),
            &opts(CodegenLang::Curl),
        );
        assert!(!c.contains("formsecret999"));
        assert!(c.contains("user=me"));
    }

    #[test]
    fn 민감하지_않은_쿠키_헤더는_보존한다() {
        let h = json_post_headers();
        let py = generate_code(&json_post(&h), &opts(CodegenLang::Python));
        assert!(py.contains("dark")); // theme cookie not masked
    }

    // ── generateCode languages ──────────────────────────────────────────────

    #[test]
    fn python은_json_바디를_data로_보내고_form으로_보내지_않는다() {
        let h = json_post_headers();
        let py = generate_code(&json_post(&h), &raw(CodegenLang::Python));
        assert!(py.contains("import requests"));
        assert!(py.contains(r#"data = "{\"item\":\"x\",\"qty\":2}""#));
        assert!(py.contains("resp = s.post("));
    }

    #[test]
    fn form_바디는_python_dict로_파싱한다() {
        let h = headers(FORM);
        let py = generate_code(
            &post("https://x.io/login", &h, "user=me&pw=secret"),
            &raw(CodegenLang::Python),
        );
        assert!(py.contains(r#""user": "me""#));
        assert!(py.contains(r#""pw": "secret""#));
    }

    #[test]
    fn curl_출력() {
        let h = json_post_headers();
        let c = generate_code(&json_post(&h), &raw(CodegenLang::Curl));
        assert!(c.contains("curl -X POST"));
        assert!(c.contains("-H 'Content-Type: application/json'"));
        assert!(c.contains("-b 'SESSIONID=abcdef1234567890; theme=dark'"));
        assert!(c.contains(r#"--data-raw '{"item":"x","qty":2}'"#));
    }

    #[test]
    fn curl은_캡처_값의_명령_치환과_백틱이_셸에서_실행되지_않게_작은따옴표로_감싼다() {
        let h = headers(&[("x-note", "it's `whoami`")]);
        let c = generate_code(
            &post(
                "https://x.io/a?q=$(id)",
                &h,
                r#"{"u":"$(touch /tmp/pwned)"}"#,
            ),
            &raw(CodegenLang::Curl),
        );
        assert!(c.contains("'https://x.io/a?q=$(id)'"));
        assert!(c.contains(r"-H 'x-note: it'\''s `whoami`'"));
        assert!(c.contains(r#"--data-raw '{"u":"$(touch /tmp/pwned)"}'"#));
    }

    #[test]
    fn python은_문자열_안의_true를_파이썬_불리언으로_바꾸지_않는다() {
        let h = headers(&[("x-note", "a: true, b: null")]);
        let py = generate_code(
            &CodegenInput {
                method: "GET",
                url: "https://x.io/",
                request_headers: Some(&h),
                request_post_data: None,
            },
            &raw(CodegenLang::Python),
        );
        assert!(py.contains(r#""x-note": "a: true, b: null""#));
    }

    #[test]
    fn typescript_출력() {
        let h = json_post_headers();
        let ts = generate_code(&json_post(&h), &raw(CodegenLang::Typescript));
        assert!(ts.contains("await fetch("));
        assert!(ts.contains(r#"method: "POST""#));
        assert!(ts.contains("credentials: 'include'"));
        assert!(ts.contains("body:"));
    }

    // ── not in the TypeScript suite: whole outputs, checked against the TypeScript ──

    #[test]
    fn python_전체_출력이_타입스크립트와_같다() {
        let h = json_post_headers();
        assert_eq!(
            generate_code(&json_post(&h), &opts(CodegenLang::Python)),
            r#"import requests

s = requests.Session()
s.headers.update({
    "Content-Type": "application/json",
    "Authorization": "Bear…7890 (33 chars)"
})
s.cookies.update({
    "SESSIONID": "abcd…7890 (16 chars)",
    "theme": "dark"
})
data = "{\"item\":\"x\",\"qty\":2}"
resp = s.post("https://api.example.com/v3/order", data=data)
print(resp.status_code, resp.text[:500])"#
        );
        let httpx = generate_code(
            &json_post(&h),
            &CodegenOptions {
                python_library: Some(PythonLibrary::Httpx),
                ..raw(CodegenLang::Python)
            },
        );
        assert!(httpx.starts_with("import httpx\n\ns = httpx.Client()\n"));
        assert!(httpx.contains(r#""Authorization": "Bearer supersecrettoken1234567890""#));
    }

    #[test]
    fn curl_전체_출력이_타입스크립트와_같다() {
        let h = json_post_headers();
        assert_eq!(
            generate_code(&json_post(&h), &opts(CodegenLang::Curl)),
            r#"curl -X POST 'https://api.example.com/v3/order' \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bear…7890 (33 chars)' \
  -b 'SESSIONID=abcd…7890 (16 chars); theme=dark' \
  --data-raw '{"item":"x","qty":2}'"#
        );
    }

    #[test]
    fn typescript_전체_출력이_타입스크립트와_같다() {
        let h = json_post_headers();
        assert_eq!(
            generate_code(&json_post(&h), &opts(CodegenLang::Typescript)),
            r#"const res = await fetch("https://api.example.com/v3/order", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
    "Authorization": "Bear…7890 (33 chars)",
    "Cookie": "SESSIONID=abcd…7890 (16 chars); theme=dark"
  },
  credentials: 'include',
  body: "{\"item\":\"x\",\"qty\":2}"
})
console.log(res.status, await res.text())"#
        );
        let h = headers(&[
            ("X-Api-Key", "abcdefghijklmnopqrstuvwxyz"),
            ("accept", "*/*"),
        ]);
        let put = CodegenInput {
            method: "put",
            url: "https://x.io/",
            request_headers: Some(&h),
            request_post_data: Some("line1\nline2 \"q\""),
        };
        assert_eq!(
            generate_code(&put, &opts(CodegenLang::Typescript)),
            r#"const res = await fetch("https://x.io/", {
  method: "PUT",
  headers: {
    "X-Api-Key": "abcd…wxyz (26 chars)",
    "accept": "*/*"
  },
  credentials: 'include',
  body: "line1\nline2 \"q\""
})
console.log(res.status, await res.text())"#
        );
    }

    #[test]
    fn 헤더도_바디도_없는_요청은_get으로_낸다() {
        let bare = CodegenInput {
            url: "https://x.io/",
            ..CodegenInput::default()
        };
        assert_eq!(
            generate_code(&bare, &opts(CodegenLang::Python)),
            "import requests\n\ns = requests.Session()\nresp = s.get(\"https://x.io/\")\nprint(resp.status_code, resp.text[:500])"
        );
        assert_eq!(
            generate_code(&bare, &opts(CodegenLang::Typescript)),
            "const res = await fetch(\"https://x.io/\", {\n  method: \"GET\",\n  headers: {},\n  credentials: 'include'\n})\nconsole.log(res.status, await res.text())"
        );
        assert_eq!(
            generate_code(&bare, &opts(CodegenLang::Curl)),
            "curl -X GET 'https://x.io/'"
        );
    }

    #[test]
    fn ir은_camel_case_필드로_직렬화한다() {
        let h = json_post_headers();
        assert_eq!(
            serde_json::to_string(&build_request_ir(&json_post(&h))).unwrap(),
            concat!(
                r#"{"method":"POST","url":"https://api.example.com/v3/order","#,
                r#""headers":{"Content-Type":"application/json","Authorization":"Bearer supersecrettoken1234567890"},"#,
                r#""cookies":{"SESSIONID":"abcdef1234567890","theme":"dark"},"#,
                r#""body":"{\"item\":\"x\",\"qty\":2}","bodyType":"json"}"#
            )
        );
    }

    #[test]
    fn 쿠키는_자바스크립트_객체_순서로_담고_같은_이름은_덮어쓴다() {
        let h = headers(&[("cookie", "a=1; noeq; b = x=y ;a=2; 3=c; 1=d")]);
        let ir = build_request_ir(&CodegenInput {
            method: "GET",
            url: "u",
            request_headers: Some(&h),
            request_post_data: None,
        });
        assert_eq!(
            serde_json::to_string(&ir).unwrap(),
            r#"{"method":"GET","url":"u","headers":{},"cookies":{"1":"d","3":"c","a":"2","b":"x=y"},"bodyType":"none"}"#
        );
    }

    #[test]
    fn 같은_이름의_쿼리_파라미터는_하나로_합쳐_마지막_값을_마스킹한다() {
        let c = generate_code(
            &CodegenInput {
                method: "GET",
                url: "https://x.io/a?access_token=querysecret123456789&page=2&q=a b&access_token=zzzzzzzzzzzzzzzzzz&k=short",
                ..CodegenInput::default()
            },
            &opts(CodegenLang::Curl),
        );
        assert_eq!(
            c,
            "curl -X GET 'https://x.io/a?access_token=zzzz%E2%80%A6zzzz+%2818+chars%29&page=2&q=a+b&k=short'"
        );
    }

    #[test]
    fn 마스킹할_것이_없는_url과_json_바디는_바이트_그대로_둔다() {
        let h = headers(JSON);
        let c = generate_code(
            &post(
                "https://x.io/login?q=a b",
                &h,
                r#"{"user": "me",  "n":1.0}"#,
            ),
            &opts(CodegenLang::Curl),
        );
        assert_eq!(
            c,
            "curl -X POST 'https://x.io/login?q=a b' \\\n  -H 'content-type: application/json' \\\n  --data-raw '{\"user\": \"me\",  \"n\":1.0}'"
        );
        // Not JSON despite the content type: left alone.
        let broken = generate_code(
            &post("https://x.io/", &h, r#"{"password": oops"#),
            &opts(CodegenLang::Curl),
        );
        assert!(broken.ends_with(r#"--data-raw '{"password": oops'"#));
    }

    #[test]
    fn 마스킹한_json_바디는_자바스크립트처럼_다시_직렬화한다() {
        let h = headers(JSON);
        let c = generate_code(
            &post(
                "https://x.io/login",
                &h,
                r#"{"user":"me", "n":1.0,"password":"hunter2hunter2","nested":{"refresh_token":"rt-0123456789abcdef","7":true,"list":[{"apiKey":"k-123456789012"},{"apiKey":5}]}}"#,
            ),
            &opts(CodegenLang::Curl),
        );
        assert!(c.ends_with(
            r#"--data-raw '{"user":"me","n":1,"password":"hunt…ter2 (14 chars)","nested":{"7":true,"refresh_token":"rt-0…cdef (19 chars)","list":[{"apiKey":"k-12…9012 (14 chars)"},{"apiKey":5}]}}'"#
        ));
    }

    #[test]
    fn 마스킹한_json_바디의_숫자는_자바스크립트와_같은_글자로_쓴다() {
        // Expected text is the TypeScript's output for the same body.
        let h = headers(JSON);
        let c = generate_code(
            &post(
                "https://x.io/",
                &h,
                r#"{"password":"hunter2hunter2","id":12345678901234567890,"a":1e21,"b":0.000001,"c":1e-7,"d":1.5e300,"e":123456789012345680000,"f":-0,"g":1e16,"h":9007199254740993,"i":0.12345678901234567,"j":-1e20,"k":1.0e5,"l":37.566535012345678,"m":0.00001234,"n":-0.0000012}"#,
            ),
            &opts(CodegenLang::Curl),
        );
        assert!(
            c.ends_with(
                r#"--data-raw '{"password":"hunt…ter2 (14 chars)","id":12345678901234567000,"a":1e+21,"b":0.000001,"c":1e-7,"d":1.5e+300,"e":123456789012345680000,"f":0,"g":10000000000000000,"h":9007199254740992,"i":0.12345678901234566,"j":-100000000000000000000,"k":100000,"l":37.566535012345675,"m":0.00001234,"n":-0.0000012}'"#
            ),
            "{c}"
        );
    }

    #[test]
    fn 마스킹한_form_바디는_uri_컴포넌트로_다시_인코딩한다() {
        let h = headers(FORM);
        let c = generate_code(
            &post(
                "https://x.io/login",
                &h,
                "user=me+you&passwd=formsecret999&x=%zz&empty&=v&a%20b=c%26d",
            ),
            &opts(CodegenLang::Curl),
        );
        assert!(c.ends_with(
            "--data-raw 'user=me%2Byou&passwd=form%E2%80%A6t999%20(13%20chars)&x=%25zz&empty=&=v&a%20b=c%26d'"
        ));
        // Nothing sensitive: the form body keeps its exact bytes.
        let plain = generate_code(
            &post("https://x.io/", &h, "user=me+you&x=%zz"),
            &opts(CodegenLang::Curl),
        );
        assert!(plain.ends_with("--data-raw 'user=me+you&x=%zz'"));
    }

    #[test]
    fn form_바디의_python_dict는_디코딩한_값을_객체_순서로_담는다() {
        let h = headers(FORM);
        let py = generate_code(
            &post(
                "https://x.io/login",
                &h,
                "user=me+you&2=b&x=%zz&empty&1=a&a%20b=c%26d",
            ),
            &raw(CodegenLang::Python),
        );
        assert!(py.contains(
            "data = {\n    \"1\": \"a\",\n    \"2\": \"b\",\n    \"user\": \"me+you\",\n    \"x\": \"%zz\",\n    \"empty\": \"\",\n    \"a b\": \"c&d\"\n}\n"
        ));
    }

    #[test]
    fn 대문자가_아닌_메서드와_해석할_수_없는_url도_안전하게_낸다() {
        let c = generate_code(
            &CodegenInput {
                method: "m-search",
                url: "not a url?token=abcdefghijklmnop",
                request_headers: None,
                request_post_data: Some("plain @text"),
            },
            &opts(CodegenLang::Curl),
        );
        assert_eq!(
            c,
            "curl -X 'M-SEARCH' 'not a url?token=abcdefghijklmnop' \\\n  --data-raw 'plain @text'"
        );
    }

    #[test]
    fn 민감한_이름은_대소문자를_가리지_않고_찾는다() {
        assert!(SENSITIVE_HEADER_RE.is_match("AUTHORIZATION"));
        assert!(SENSITIVE_HEADER_RE.is_match("x-csrf-token"));
        // Anchored: a header that merely contains a sensitive name is not masked.
        assert!(!SENSITIVE_HEADER_RE.is_match("x-authorization-hint"));
        assert!(SENSITIVE_FIELD_RE.is_match("userPassWd"));
        assert!(SENSITIVE_FIELD_RE.is_match("PHPSESSID"));
        assert!(!SENSITIVE_FIELD_RE.is_match("theme"));
    }
}
