//! Port of `src/shared/cli-spec.ts` — captured calls become a site CLI. Algorithms follow
//! Hybirdss/Hycli (Apache-2.0) src/capture/mod.rs and src/policy.rs: cluster a recording,
//! template dynamic path segments, drop static assets and secret field names,
//! and classify effect without trusting a caller-supplied label.
//! The fork greekr4/Hycli matched that upstream at the time of the port.
//! Values, cookies, and machine paths are not stored.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;
use url::{Url, form_urlencoded};

/// A JavaScript object used as a string-keyed map. Its key order is output:
/// flags are described, exported and sent in it.
pub type Record<V> = IndexMap<String, V>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliEffect {
    Read,
    Write,
    Unknown,
}

impl fmt::Display for CliEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Unknown => "unknown",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliParamKind {
    String,
    Int,
    Bool,
}

impl fmt::Display for CliParamKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::String => "string",
            Self::Int => "int",
            Self::Bool => "bool",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliParam {
    pub kind: CliParamKind,
    pub required: bool,
}

/// One value read from each row the item selector matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliExtractField {
    pub name: String,
    /// CSS selector relative to the row. Omit to use the row itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// Attribute name. Omit to read the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,
}

/// Fields to pull out of an HTML response instead of returning the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliExtract {
    pub selector: String,
    /// Default 5. Maximum 20.
    pub limit: u32,
    pub fields: Vec<CliExtractField>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliBody {
    #[serde(
        default,
        deserialize_with = "saved_optional_flags",
        skip_serializing_if = "Option::is_none"
    )]
    pub json: Option<Record<CliParam>>,
    #[serde(
        default,
        deserialize_with = "saved_optional_flags",
        skip_serializing_if = "Option::is_none"
    )]
    pub form: Option<Record<CliParam>>,
}

/// Flags as `JSON.parse` hands them to the TypeScript: whatever order the file
/// has them in, a JavaScript object lists array-index names first.
fn saved_flags<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Record<CliParam>, D::Error> {
    Record::deserialize(deserializer).map(js_key_order)
}

fn saved_optional_flags<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Record<CliParam>>, D::Error> {
    Ok(Option::<Record<CliParam>>::deserialize(deserializer)?.map(js_key_order))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliCommand {
    pub name: String,
    pub desc: String,
    pub method: String,
    pub path: String,
    pub effect: CliEffect,
    pub evidence_request_ids: Vec<String>,
    #[serde(deserialize_with = "saved_flags")]
    pub params: Record<CliParam>,
    #[serde(deserialize_with = "saved_flags")]
    pub query: Record<CliParam>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<CliBody>,
    pub session_headers: Vec<String>,
    pub response_pointers: Vec<String>,
    /// Saved once. Later runs return these fields instead of the raw page.
    #[serde(
        default,
        deserialize_with = "saved_extract",
        skip_serializing_if = "Option::is_none"
    )]
    pub extract: Option<CliExtract>,
}

/// A saved command whose extract is broken still loads, without the extract:
/// what the TypeScript callers get by passing every loaded command through
/// `attachExtract`.
fn saved_extract<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<CliExtract>, D::Error> {
    let raw = Value::deserialize(deserializer)?;
    Ok(parse_extract(&raw).ok().flatten())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliData {
    pub site: String,
    pub title: String,
    pub base_url: String,
    pub commands: Vec<CliCommand>,
    pub writes: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedCall {
    pub request_id: String,
    pub method: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<Record<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_post_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Default 20. Values below 1 or above 100 become 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Command name per requestId, e.g. { "123.4": "search" }. Slugged; empty falls back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<Record<String>>,
}

pub const WRITES_DISABLED: &str = "writes disabled for this CLI";
pub const OUTCOME_UNKNOWN: &str = "action_outcome_unknown";

const STATIC_EXT: &[&str] = &[
    ".js", ".css", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".woff", ".woff2", ".ico", ".map",
];

const DROPPED_TYPES: &[&str] = &[
    "script",
    "stylesheet",
    "image",
    "font",
    "media",
    "texttrack",
    "manifest",
];

const CHANGES: &[&str] = &[
    "add",
    "edit",
    "change",
    "toggle",
    "commit",
    "upsert",
    "redeem",
    "confirm",
    "authorize",
    "login",
    "signin",
    "logout",
    "logoff",
    "signout",
    "delete",
    "remove",
    "destroy",
    "erase",
    "purge",
    "drop",
    "create",
    "insert",
    "update",
    "patch",
    "submit",
    "send",
    "publish",
    "unpublish",
    "upload",
    "transfer",
    "purchase",
    "checkout",
    "pay",
    "vote",
    "like",
    "unlike",
    "follow",
    "unfollow",
    "subscribe",
    "unsubscribe",
    "revoke",
    "reset",
    "activate",
    "deactivate",
    "enable",
    "disable",
    "execute",
    "trigger",
    "accept",
    "reject",
    "approve",
    "invite",
    "cancel",
    "terminate",
    "stop",
    "restart",
    "save",
    "markread",
    "markseen",
    "increment",
];

const SECRET_WORDS: &[&str] = &[
    "tokens",
    "cookies",
    "session",
    "auth",
    "token",
    "authtoken",
    "csrf",
    "xsrf",
    "credential",
    "credentials",
    "password",
    "passwd",
    "authorization",
    "cookie",
    "setcookie",
    "accesstoken",
    "refreshtoken",
    "idtoken",
    "csrftoken",
    "xsrftoken",
    "apikey",
    "secret",
    "clientsecret",
    "sessiontoken",
    "sessionid",
    "bearer",
];

fn fixed_regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("fixed pattern compiles")
}

// `(?-u:\b)` and the spelled-out class keep JavaScript's meaning of `\b` and `\s`:
// the regex crate's `\b` counts non-ASCII letters as word characters, and its
// `\s` has U+0085 but not U+FEFF.
static BEARER_RE: LazyLock<Regex> = LazyLock::new(|| {
    fixed_regex(
        r"(?-u:\b)Bearer[\t\n\x0B\x0C\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]+[A-Za-z0-9._~+/-]{8,}=*",
    )
});
static JWT_RE: LazyLock<Regex> =
    LazyLock::new(|| fixed_regex(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+"));
static MUTATION_RE: LazyLock<Regex> = LazyLock::new(|| fixed_regex(r"(?-u:\b)mutation(?-u:\b)"));
static PLACEHOLDER_RE: LazyLock<Regex> = LazyLock::new(|| fixed_regex(r"\{([^}]+)\}"));

/// JavaScript's whitespace set (`String.prototype.trim`).
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

/// JavaScript's `string.length`.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JavaScript's `text.slice(0, units)`. A surrogate pair split by the cut is
/// left out whole, since a Rust string can't hold the lone half.
fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut seen = 0;
    let end = text
        .char_indices()
        .find(|(_, c)| {
            seen += c.len_utf16();
            seen > units
        })
        .map_or(text.len(), |(at, _)| at);
    text.get(..end).unwrap_or(text)
}

fn has_control(text: &str) -> bool {
    text.chars().any(|c| c <= '\u{1f}')
}

fn non_empty(text: &Option<String>) -> Option<&str> {
    text.as_deref().filter(|text| !text.is_empty())
}

/// `decodeURIComponent`: None where JavaScript throws, on a `%` without two hex
/// digits or on escapes that are not UTF-8.
fn decode_uri_component(text: &str) -> Option<String> {
    let hex = |byte: Option<u8>| byte.and_then(|b| (b as char).to_digit(16));
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let (high, low) = (hex(bytes.next())?, hex(bytes.next())?);
            out.push((high * 16 + low) as u8);
        } else {
            out.push(byte);
        }
    }
    String::from_utf8(out).ok()
}

/// `url.pathname`. The URL standard, and so Node and Chromium, writes a "^" in
/// a hierarchical path as "%5E"; the url crate leaves it as it is.
fn pathname(url: &Url) -> String {
    if url.cannot_be_a_base() {
        url.path().to_owned()
    } else {
        url.path().replace('^', "%5E")
    }
}

/// `encodeURIComponent`.
fn encode_uri_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The key as an array index (a canonical integer below 2^32 - 1), if it is one.
fn array_index(key: &str) -> Option<u32> {
    let canonical =
        key == "0" || (!key.starts_with('0') && key.bytes().all(|b| b.is_ascii_digit()));
    key.parse::<u32>()
        .ok()
        .filter(|n| canonical && *n != u32::MAX)
}

/// The order a JavaScript object lists its keys in: array indices ascending,
/// then the other keys in insertion order.
fn js_key_order<V>(mut map: Record<V>) -> Record<V> {
    // Stable, so keys that are not indices keep their insertion order.
    map.sort_by(|a, _, b, _| {
        let rank = |key: &str| array_index(key).map_or((1, 0), |index| (0, index));
        rank(a).cmp(&rank(b))
    });
    map
}

/// The shortest digits that read back as `value`, and the power of ten of the
/// first one. None for an infinity.
fn shortest_digits(value: f64) -> Option<(String, i32)> {
    let parts = |text: String| -> Option<(String, i32)> {
        let (mantissa, exponent) = text.split_once('e')?;
        Some((mantissa.replace('.', ""), exponent.parse().ok()?))
    };
    let (digits, exponent) = parts(format!("{value:e}"))?;
    // Where two digit strings of that length are equally near, `{:e}` takes the
    // upper one and JavaScript the one ending in an even digit. A tie shows as an
    // exact expansion one digit longer that ends in 5; 767 digits hold any double.
    let (exact, exact_exponent) = parts(format!("{value:.766e}"))?;
    let exact = exact.trim_end_matches('0');
    if exact.len() == digits.len() + 1 && exact.ends_with('5') {
        let (head, last) = exact
            .get(..digits.len())?
            .split_at_checked(digits.len() - 1)?;
        let last = last.parse::<u8>().ok()?;
        // An odd 9 would carry into a shorter string, which `{:e}` would have found.
        if last != 9 {
            let even = format!("{head}{}", last + last % 2);
            let shift = exact_exponent - (digits.len() as i32 - 1);
            if format!("{even}e{shift}").parse() == Ok(value) {
                return Some((even, exact_exponent));
            }
        }
    }
    Some((digits, exponent))
}

/// `String(number)`.
fn js_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    let Some((digits, exponent)) = shortest_digits(value.abs()) else {
        // Only an out-of-range JSON number gets here.
        return format!("{sign}Infinity");
    };
    let k = digits.len() as i32;
    let n = exponent + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        let (whole, fraction) = digits.split_at(n as usize);
        format!("{whole}.{fraction}")
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat(-n as usize))
    } else {
        let (first, rest) = digits.split_at(1);
        let point = if rest.is_empty() { "" } else { "." };
        let exponent_sign = if n > 0 { '+' } else { '-' };
        format!("{first}{point}{rest}e{exponent_sign}{}", (n - 1).abs())
    };
    format!("{sign}{body}")
}

/// `JSON.parse(text)` when it yields a plain object: the top-level entries in
/// JavaScript's key order, each value still as its source text.
fn json_object(text: &str) -> Option<Record<Box<RawValue>>> {
    serde_json::from_str(text).ok().map(js_key_order)
}

/// `String(value)` for a JSON scalar. None for null, objects and arrays.
fn json_scalar_text(raw: &RawValue) -> Option<String> {
    let text = raw.get();
    match text.as_bytes().first()? {
        b'n' | b'{' | b'[' => None,
        // shortcut: a string holding a lone surrogate escape reads as empty, since Rust
        // can't hold it; upgrade if such values ever need to show in the panel.
        b'"' => Some(serde_json::from_str(text).unwrap_or_default()),
        b't' | b'f' => Some(text.to_owned()),
        _ => text.parse().ok().map(js_number),
    }
}

pub fn dangerous_intent(value: &str) -> bool {
    let decoded =
        decode_uri_component(&value.replace('+', " ")).unwrap_or_else(|| value.to_owned());
    let mut split = String::with_capacity(decoded.len());
    let mut previous = '\0';
    for c in decoded.chars() {
        if c.is_ascii_uppercase() && (previous.is_ascii_lowercase() || previous.is_ascii_digit()) {
            split.push('-');
        }
        split.push(c);
        previous = c;
    }
    let lower = split.to_lowercase();
    lower
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .any(|word| CHANGES.contains(&word))
        || ["sign-out", "log-out", "mark-as-read", "mark-as-seen"]
            .iter()
            .any(|phrase| lower.contains(phrase))
}

pub fn secret_field(key: &str) -> bool {
    let lower = key.to_lowercase().replace(['-', '_'], "");
    SECRET_WORDS.iter().any(|word| lower.ends_with(word))
}

pub fn static_asset(path: &str) -> bool {
    let lower = path.to_lowercase();
    STATIC_EXT.iter().any(|ext| lower.ends_with(ext))
}

pub fn scrub_secrets(text: &str) -> String {
    let bearer = BEARER_RE.replace_all(text, "Bearer ****");
    JWT_RE.replace_all(&bearer, "****").into_owned()
}

pub fn run_gate(effect: CliEffect, writes: bool) -> Option<&'static str> {
    (effect != CliEffect::Read && !writes).then_some(WRITES_DISABLED)
}

fn limit_of(limit: Option<i64>) -> usize {
    limit
        .filter(|limit| (1..=100).contains(limit))
        .map_or(20, |limit| limit as usize)
}

/// Every run of characters outside `a-z0-9` becomes one dash; one dash is then
/// dropped from each end.
fn dash_slug(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for c in text.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    trim_dash(&slug).to_owned()
}

fn trim_dash(text: &str) -> &str {
    let text = text.strip_prefix('-').unwrap_or(text);
    text.strip_suffix('-').unwrap_or(text)
}

fn slug_site(host: &str) -> String {
    let slug = dash_slug(host);
    if slug.is_empty() {
        "site".to_owned()
    } else {
        slug
    }
}

fn is_dynamic(seg: &str) -> bool {
    if seg.is_empty() {
        return false;
    }
    if seg.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    if utf16_len(seg) == 36 && seg.split('-').count() == 5 {
        return true;
    }
    seg.len() >= 16 && seg.bytes().all(|b| b.is_ascii_hexdigit())
}

fn templify(pathname: &str) -> String {
    let mut idx = 0;
    pathname
        .split('/')
        .map(|seg| {
            if !is_dynamic(seg) {
                return seg.to_owned();
            }
            let name = format!("{{id{idx}}}");
            idx += 1;
            name
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn slugify(method: &str, template: &str) -> String {
    let mut body = String::with_capacity(template.len());
    for c in template.to_lowercase().chars() {
        if matches!(c, '{' | '}' | '/' | '-') {
            if !body.ends_with('-') {
                body.push('-');
            }
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
            body.push(c);
        }
    }
    let body = trim_dash(&body);
    format!(
        "{}-{}",
        method.to_lowercase(),
        if body.is_empty() { "root" } else { body }
    )
}

fn command_slug(raw: &str) -> String {
    let slug = dash_slug(raw);
    // ASCII by now, so the 40-unit cut is a byte cut.
    let cut = slug.get(..40).unwrap_or(&slug);
    cut.strip_suffix('-').unwrap_or(cut).to_owned()
}

fn chosen_name(ids: &[String], names: Option<&Record<String>>) -> String {
    ids.iter()
        .filter_map(|id| names?.get(id))
        .find(|raw| !raw.is_empty())
        .map(|raw| command_slug(raw))
        .unwrap_or_default()
}

fn param_kind(value: &str) -> CliParamKind {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        CliParamKind::Int
    } else if value == "true" || value == "false" {
        CliParamKind::Bool
    } else {
        CliParamKind::String
    }
}

fn header_name(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let kept = lower != "cookie"
        && lower != "authorization"
        && (lower.contains("csrf") || lower.contains("xsrf"))
        && (1..=80).contains(&lower.len())
        && lower
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    kept.then_some(lower)
}

fn json_pointers(body: Option<&str>) -> Vec<String> {
    json_object(body.unwrap_or(""))
        .unwrap_or_default()
        .keys()
        .filter(|key| !secret_field(key))
        .take(12)
        .map(|key| format!("/{key}"))
        .collect()
}

fn looks_json(call: &CapturedCall) -> bool {
    if call
        .mime_type
        .as_deref()
        .is_some_and(|mime| mime.to_lowercase().contains("json"))
    {
        return true;
    }
    let body = js_trim(call.response_body.as_deref().unwrap_or(""));
    body.starts_with('{') || body.starts_with('[')
}

#[derive(Default)]
struct Cluster {
    method: String,
    origin: String,
    template: String,
    first_index: usize,
    count: usize,
    last_status: u16,
    is_json: bool,
    ids: Vec<String>,
    query: Record<CliParamKind>,
    json_body: Record<CliParamKind>,
    form_body: Record<CliParamKind>,
    session_headers: BTreeSet<String>,
    pointers: Vec<String>,
    raw_for_effect: String,
}

fn add_fields<K: AsRef<str>, V: AsRef<str>>(
    target: &mut Record<CliParamKind>,
    fields: impl IntoIterator<Item = (K, V)>,
) {
    for (key, value) in fields {
        let key = key.as_ref();
        if key.is_empty() || secret_field(key) || target.contains_key(key) {
            continue;
        }
        target.insert(key.to_owned(), param_kind(value.as_ref()));
    }
}

fn effect_for(method: &str, template: &str, name: &str, raw: &str) -> CliEffect {
    if dangerous_intent(template) || dangerous_intent(name) || dangerous_intent(raw) {
        return CliEffect::Write;
    }
    let upper = method.to_uppercase();
    if matches!(upper.as_str(), "PUT" | "PATCH" | "DELETE") || MUTATION_RE.is_match(raw) {
        return CliEffect::Write;
    }
    match upper.as_str() {
        "GET" | "HEAD" | "OPTIONS" => CliEffect::Read,
        "POST" => CliEffect::Write,
        _ => CliEffect::Unknown,
    }
}

/// The fields of a captured request body.
enum PostBody {
    Form(Vec<(String, String)>),
    /// Top-level entries of a JSON object. A null, object or array value is None.
    Json(Vec<(String, Option<String>)>),
    /// Empty, or a body that is not a field map.
    Other,
}

fn read_post(post: &str, headers: Option<&Record<String>>) -> PostBody {
    if post.is_empty() {
        return PostBody::Other;
    }
    let content_type = headers
        .into_iter()
        .flatten()
        .find(|(key, _)| key.to_lowercase() == "content-type")
        .map_or("", |(_, value)| value.as_str());
    if content_type.contains("application/x-www-form-urlencoded")
        || (!js_trim(post).starts_with('{') && post.contains('='))
    {
        // `new URLSearchParams(text)` skips one leading "?".
        let pairs = post.strip_prefix('?').unwrap_or(post);
        return PostBody::Form(
            form_urlencoded::parse(pairs.as_bytes())
                .into_owned()
                .collect(),
        );
    }
    match json_object(post) {
        Some(entries) => PostBody::Json(
            entries
                .iter()
                .map(|(key, raw)| (key.clone(), json_scalar_text(raw)))
                .collect(),
        ),
        None => PostBody::Other,
    }
}

fn absorb(cluster: &mut Cluster, call: &CapturedCall, url: &Url) {
    cluster.count += 1;
    cluster.last_status = call.status.unwrap_or(cluster.last_status);
    cluster.ids.push(call.request_id.clone());
    if looks_json(call) {
        cluster.is_json = true;
    }
    add_fields(&mut cluster.query, url.query_pairs());
    let headers = call.request_headers.as_ref();
    for name in headers.into_iter().flat_map(|headers| headers.keys()) {
        if let Some(kept) = header_name(name) {
            cluster.session_headers.insert(kept);
        }
    }
    let pointers = json_pointers(call.response_body.as_deref());
    if pointers.len() > cluster.pointers.len() {
        cluster.pointers = pointers;
    }
    let post = call.request_post_data.as_deref().unwrap_or("");
    let search = url.query().filter(|query| !query.is_empty());
    cluster.raw_for_effect.push('\n');
    if let Some(query) = search {
        cluster.raw_for_effect.push('?');
        cluster.raw_for_effect.push_str(query);
    }
    cluster.raw_for_effect.push('\n');
    cluster.raw_for_effect.push_str(post);
    match read_post(post, headers) {
        PostBody::Form(pairs) => add_fields(&mut cluster.form_body, pairs),
        PostBody::Json(entries) => add_fields(
            &mut cluster.json_body,
            entries
                .into_iter()
                .map(|(key, value)| (key, value.unwrap_or_default())),
        ),
        // A body that is not a field map stays out of the flags.
        PostBody::Other => {}
    }
}

fn to_params(fields: &Record<CliParamKind>, required: bool) -> Record<CliParam> {
    js_key_order(
        fields
            .iter()
            .map(|(key, kind)| {
                (
                    key.clone(),
                    CliParam {
                        kind: *kind,
                        required,
                    },
                )
            })
            .collect(),
    )
}

/// Err is the message of the error the TypeScript throws: `baseUrl`, or the
/// origin the site name would come from, is not a URL.
pub fn compile_cli(calls: &[CapturedCall], opts: &CompileOptions) -> Result<CliData, String> {
    const INVALID_URL: &str = "Invalid URL";
    let limit = limit_of(opts.limit);
    let origin_filter = match non_empty(&opts.base_url) {
        Some(base_url) => Some(
            Url::parse(base_url)
                .map_err(|_| INVALID_URL)?
                .origin()
                .ascii_serialization(),
        ),
        None => None,
    };
    let mut groups: IndexMap<String, Cluster> = IndexMap::new();
    for (index, call) in calls.iter().enumerate() {
        if js_trim(&call.request_id).is_empty() {
            continue;
        }
        let Ok(url) = Url::parse(&call.url) else {
            continue;
        };
        let origin = url.origin().ascii_serialization();
        if origin_filter.as_ref().is_some_and(|only| *only != origin) {
            continue;
        }
        let resource_type = call.resource_type.as_deref().unwrap_or("").to_lowercase();
        let path = pathname(&url);
        if DROPPED_TYPES.contains(&resource_type.as_str()) || static_asset(&path) {
            continue;
        }
        let method = if call.method.is_empty() {
            "GET"
        } else {
            &call.method
        }
        .to_uppercase();
        let template = templify(if path.is_empty() { "/" } else { &path });
        let cluster = groups
            .entry(format!("{method} {origin} {template}"))
            .or_insert_with(|| Cluster {
                method,
                origin,
                template,
                first_index: index,
                last_status: call.status.unwrap_or(0),
                ..Cluster::default()
            });
        absorb(cluster, call, &url);
    }

    let mut clusters: Vec<Cluster> = groups.into_values().collect();
    if clusters.len() > limit {
        clusters.sort_by(|a, b| {
            b.is_json
                .cmp(&a.is_json)
                .then(a.first_index.cmp(&b.first_index))
        });
        clusters.truncate(limit);
    } else {
        clusters.sort_by_key(|cluster| cluster.first_index);
    }

    let base = match origin_filter {
        Some(origin) => origin,
        None => clusters
            .first()
            .map(|cluster| cluster.origin.clone())
            .unwrap_or_default(),
    };
    let site = slug_site(match non_empty(&opts.site) {
        Some(site) => site,
        None if base.is_empty() => "site",
        // `new URL(base).host`. An origin is `scheme://host[:port]`, or "null"
        // for a file: or data: URL, which does not parse.
        None => base.split_once("://").ok_or(INVALID_URL)?.1,
    });
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut commands = Vec::new();
    for cluster in clusters {
        if cluster.ids.is_empty() {
            continue;
        }
        // The effect is judged on the derived name, so a friendly name chosen by
        // the caller can never turn a write into a read.
        let derived = slugify(&cluster.method, &cluster.template);
        let mut name = chosen_name(&cluster.ids, opts.names.as_ref());
        if name.is_empty() {
            name.clone_from(&derived);
        }
        while used.contains(&name) {
            name.push_str("-x");
        }
        used.insert(name.clone());
        let params = js_key_order(
            PLACEHOLDER_RE
                .captures_iter(&cluster.template)
                .filter_map(|found| found.get(1))
                .filter(|key| !secret_field(key.as_str()))
                .map(|key| {
                    let param = CliParam {
                        kind: CliParamKind::String,
                        required: true,
                    };
                    (key.as_str().to_owned(), param)
                })
                .collect(),
        );
        let json = to_params(&cluster.json_body, false);
        let form = to_params(&cluster.form_body, false);
        let body = if !json.is_empty() {
            Some(CliBody {
                json: Some(json),
                form: None,
            })
        } else if !form.is_empty() {
            Some(CliBody {
                json: None,
                form: Some(form),
            })
        } else {
            None
        };
        commands.push(CliCommand {
            name,
            desc: format!(
                "Observed {} {} time(s), last status {}.",
                cluster.method, cluster.count, cluster.last_status
            ),
            effect: effect_for(
                &cluster.method,
                &cluster.template,
                &derived,
                &cluster.raw_for_effect,
            ),
            method: cluster.method,
            path: cluster.template,
            evidence_request_ids: cluster.ids,
            params,
            query: to_params(&cluster.query, false),
            body,
            session_headers: cluster.session_headers.into_iter().collect(),
            response_pointers: cluster.pointers,
            extract: None,
        });
    }

    Ok(CliData {
        title: non_empty(&opts.title).unwrap_or(&site).to_owned(),
        site,
        base_url: base,
        commands,
        writes: false,
    })
}

fn json_fields(command: &CliCommand) -> Option<&Record<CliParam>> {
    command.body.as_ref()?.json.as_ref()
}

fn form_fields(command: &CliCommand) -> Option<&Record<CliParam>> {
    command.body.as_ref()?.form.as_ref()
}

fn flag_lines(command: &CliCommand) -> Vec<String> {
    let groups = [
        ("path", Some(&command.params)),
        ("query", Some(&command.query)),
        ("json", json_fields(command)),
        ("form", form_fields(command)),
    ];
    let mut lines = Vec::new();
    for (prefix, fields) in groups {
        for (name, param) in fields.into_iter().flatten() {
            let required = if param.required { " required" } else { "" };
            lines.push(format!("  --{name} {}{required} {prefix}", param.kind));
        }
    }
    lines
}

pub fn command_flag_names(command: &CliCommand) -> Vec<&str> {
    command
        .params
        .keys()
        .chain(command.query.keys())
        .chain(
            json_fields(command)
                .into_iter()
                .flat_map(|fields| fields.keys()),
        )
        .chain(
            form_fields(command)
                .into_iter()
                .flat_map(|fields| fields.keys()),
        )
        .map(String::as_str)
        .collect()
}

/// A flag value safe to show in the panel. Secrets and cookies never qualify.
fn safe_flag_value(value: &str) -> Option<&str> {
    let trimmed = js_trim(value);
    let safe = !trimmed.is_empty()
        && utf16_len(trimmed) <= 500
        && scrub_secrets(trimmed) == trimmed
        && !has_control(trimmed);
    safe.then_some(trimmed)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedFlagSource {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_post_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<Record<String>>,
}

/// The name in a `{name}` path segment.
fn placeholder_name(seg: &str) -> Option<&str> {
    seg.strip_prefix('{')?
        .strip_suffix('}')
        .filter(|name| !name.is_empty() && !name.contains('}'))
}

/// Flag values still visible on the captured call. Used to fill the call example
/// in the panel. Not written onto the saved command.
pub fn captured_flag_values(command: &CliCommand, calls: &[CapturedFlagSource]) -> Record<String> {
    let names = command_flag_names(command);
    let mut out: Record<String> = Record::new();
    let mut take = |key: &str, value: &str| {
        if !names.contains(&key) || out.contains_key(key) || secret_field(key) {
            return;
        }
        if let Some(safe) = safe_flag_value(value) {
            out.insert(key.to_owned(), safe.to_owned());
        }
    };
    let template_segs: Vec<&str> = command.path.split('/').collect();
    for call in calls {
        let Ok(url) = Url::parse(&call.url) else {
            continue;
        };
        let path = pathname(&url);
        let path_segs: Vec<&str> = path.split('/').collect();
        if template_segs.len() == path_segs.len() {
            for (template, seg) in template_segs.iter().zip(&path_segs) {
                if let Some(name) = placeholder_name(template) {
                    // A broken escape stays as the raw segment.
                    let decoded = decode_uri_component(seg).unwrap_or_else(|| (*seg).to_owned());
                    take(name, &decoded);
                }
            }
        }
        for (key, value) in url.query_pairs() {
            take(&key, &value);
        }
        let post = call.request_post_data.as_deref().unwrap_or("");
        match read_post(post, call.request_headers.as_ref()) {
            PostBody::Form(pairs) => {
                for (key, value) in pairs {
                    take(&key, &value);
                }
            }
            PostBody::Json(entries) => {
                for (key, value) in entries {
                    if let Some(value) = value {
                        take(&key, &value);
                    }
                }
            }
            // A body that is not a field map adds no example values.
            PostBody::Other => {}
        }
    }
    js_key_order(out)
}

#[derive(Serialize)]
struct RunExample<'a> {
    id: &'a str,
    command: &'a str,
    inputs: Record<&'a str>,
}

/// The cli_run object for one command. Blank flags stay empty strings. No cookies.
pub fn cli_run_example(id: &str, command: &CliCommand, inputs: &Record<String>) -> String {
    let filled = command_flag_names(command)
        .into_iter()
        .map(|name| (name.to_owned(), inputs.get(name).map_or("", String::as_str)))
        .collect();
    let example = RunExample {
        id,
        command: &command.name,
        inputs: js_key_order(filled),
    };
    serde_json::to_string_pretty(&example).unwrap_or_default()
}

fn on_off(writes: bool) -> &'static str {
    if writes { "on" } else { "off" }
}

pub fn describe_cli(data: &CliData, command_name: Option<&str>) -> String {
    let command_name = command_name.filter(|name| !name.is_empty());
    let commands: Vec<&CliCommand> = data
        .commands
        .iter()
        .filter(|command| command_name.is_none_or(|name| command.name == name))
        .collect();
    let mut lines = vec![
        format!("{}  {}", data.site, data.base_url),
        format!("writes: {}", on_off(data.writes)),
        String::new(),
    ];
    if let Some(name) = command_name
        && commands.is_empty()
    {
        let available: Vec<&str> = data.commands.iter().map(|c| c.name.as_str()).collect();
        lines.push(format!("Unknown action: {name}"));
        lines.push(format!("Available actions: {}", available.join(", ")));
        return lines.join("\n");
    }
    for command in commands {
        lines.push(command.name.clone());
        lines.push(format!("  {} {}", command.method, command.path));
        lines.push(format!("  effect: {}", command.effect));
        if let Some(extract) = &command.extract {
            lines.push(format!(
                "  extract: {}  limit {}",
                extract.selector, extract.limit
            ));
            for field in &extract.fields {
                let how = match non_empty(&field.attr) {
                    Some(attr) => format!("attr {attr}"),
                    None => "text".to_owned(),
                };
                let selector = non_empty(&field.selector)
                    .map(|selector| format!(" {selector}"))
                    .unwrap_or_default();
                lines.push(format!("    {} {how}{selector}", field.name));
            }
        }
        lines.extend(flag_lines(command));
        if !command.session_headers.is_empty() {
            lines.push(format!(
                "  session headers: {}",
                command.session_headers.join(", ")
            ));
        }
        lines.push(format!(
            "  evidence: {}",
            command.evidence_request_ids.join(", ")
        ));
        lines.push(String::new());
    }
    lines.join("\n")
}

pub fn export_curl(data: &CliData) -> String {
    let mut lines = vec![
        format!("# {} {}", data.site, data.base_url),
        format!("# writes: {}", on_off(data.writes)),
        "# BASE is the site origin. Flags are placeholders. Captured secrets are not included."
            .to_owned(),
        "# UA is your client's User-Agent, e.g. \"my-tool/1.0 (you@example.com)\". Some APIs require one."
            .to_owned(),
        String::new(),
    ];
    let form_pairs = |fields: &Record<CliParam>| {
        fields
            .keys()
            .map(|name| format!("{}=${name}", encode_uri_component(name)))
            .collect::<Vec<_>>()
            .join("&")
    };
    for command in &data.commands {
        // "$$" is a literal dollar sign in the replacement syntax.
        let path = PLACEHOLDER_RE.replace_all(&command.path, "$$${1}");
        let query = form_pairs(&command.query);
        let question = if query.is_empty() { "" } else { "?" };
        let url = format!("$BASE{path}{question}{query}");
        let json = json_fields(command).filter(|fields| !fields.is_empty());
        let form = form_fields(command).filter(|fields| !fields.is_empty());
        let extra = if let Some(json) = json {
            let pairs: Vec<String> = json
                .keys()
                .map(|name| format!("\"{name}\":\"${name}\""))
                .collect();
            format!(
                " -H 'content-type: application/json' --data '{{{}}}'",
                pairs.join(",")
            )
        } else if let Some(form) = form {
            format!(" --data '{}'", form_pairs(form))
        } else {
            String::new()
        };
        lines.push(format!(
            "# {}  {} {}  {}",
            command.name, command.method, command.path, command.effect
        ));
        if let Some(extract) = &command.extract {
            let names: Vec<&str> = extract.fields.iter().map(|f| f.name.as_str()).collect();
            lines.push(format!("# reads {}: {}", extract.limit, names.join(", ")));
            lines.push(format!("# selector: {}", extract.selector));
        }
        lines.push(format!(
            "curl -sS -A \"$UA\" -X {} \"{url}\"{extra}",
            command.method
        ));
        lines.push(String::new());
    }
    lines.join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltCliRequest {
    pub url: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// `URLSearchParams.set`: the first pair of that name takes the value and the
/// later ones go; a new name is appended.
fn set_pair(pairs: &mut Vec<(String, String)>, name: &str, value: &str) {
    let mut found = false;
    pairs.retain_mut(|(key, current)| {
        if key != name {
            return true;
        }
        if found {
            return false;
        }
        found = true;
        value.clone_into(current);
        true
    });
    if !found {
        pairs.push((name.to_owned(), value.to_owned()));
    }
}

/// Err is `missing <flag>`, or the message of the error the TypeScript throws
/// when the base URL does not parse.
pub fn build_cli_request(
    base_url: &str,
    command: &CliCommand,
    inputs: &Record<String>,
) -> Result<BuiltCliRequest, String> {
    let input = |name: &str| inputs.get(name).filter(|value| !value.is_empty());
    let missing = |name: &str| format!("missing {name}");
    let mut path = command.path.clone();
    for name in command.params.keys() {
        let value = input(name).ok_or_else(|| missing(name))?;
        path = path.replace(&format!("{{{name}}}"), &encode_uri_component(value));
    }
    let base = if base_url.ends_with('/') {
        base_url.to_owned()
    } else {
        format!("{base_url}/")
    };
    let mut url = Url::parse(&base)
        .and_then(|base| base.join(&path))
        .map_err(|_| "Invalid URL")?;
    let mut pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let mut query_set = false;
    for (name, param) in &command.query {
        if let Some(value) = input(name) {
            set_pair(&mut pairs, name, value);
            query_set = true;
        } else if param.required {
            return Err(missing(name));
        }
    }
    if query_set {
        url.query_pairs_mut().clear().extend_pairs(&pairs);
    }
    // The fields in `fields` that have a value, or the first required one that has none.
    let filled = |fields: &Record<CliParam>| -> Result<Record<String>, String> {
        let mut payload = Record::new();
        for (name, param) in fields {
            if let Some(value) = input(name) {
                payload.insert(name.clone(), value.clone());
            } else if param.required {
                return Err(missing(name));
            }
        }
        Ok(payload)
    };
    let json = json_fields(command).filter(|fields| !fields.is_empty());
    let form = form_fields(command).filter(|fields| !fields.is_empty());
    let mut body = None;
    if let Some(json) = json {
        let payload = js_key_order(filled(json)?);
        if !payload.is_empty() {
            body = serde_json::to_string(&payload).ok();
        }
    } else if let Some(form) = form {
        let payload = filled(form)?;
        if !payload.is_empty() {
            body = Some(
                form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(&payload)
                    .finish(),
            );
        }
    }
    if url.path().contains('^') && !url.cannot_be_a_base() {
        url.set_path(&pathname(&url));
    }
    Ok(BuiltCliRequest {
        url: url.into(),
        method: command.method.clone(),
        body: body.filter(|body| !body.is_empty()),
    })
}

pub const RAW_PREVIEW_CHARS: usize = 1_500;
pub const EXTRACT_MAX_CHARS: usize = 2_000_000;
pub const EXTRACT_FETCH_TIMEOUT_MS: u64 = 15_000;
pub const EXTRACT_NOTE_NONE: &str = "selector matched nothing";
pub const EXTRACT_NOTE_CAPPED: &str = "page read was capped";
const FIELD_VALUE_MAX: usize = 500;
static FIELD_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| fixed_regex(r"^[A-Za-z][A-Za-z0-9_-]{0,40}$"));
static ATTR_RE: LazyLock<Regex> =
    LazyLock::new(|| fixed_regex(r"^[A-Za-z_:][A-Za-z0-9_:-]{0,40}$"));

/// `raw` is the untyped value as saved or as an agent passed it. Ok(None) when
/// there is no extract.
pub fn parse_extract(raw: &Value) -> Result<Option<CliExtract>, String> {
    let row = match raw {
        Value::Null => return Ok(None),
        Value::Object(row) => row,
        _ => return Err("extract must be an object".to_owned()),
    };
    let selector = row
        .get("selector")
        .and_then(Value::as_str)
        .map(js_trim)
        .filter(|selector| !selector.is_empty())
        .ok_or("extract.selector is required")?;
    if utf16_len(selector) > 400 || has_control(selector) {
        return Err("extract.selector is invalid".to_owned());
    }
    let limit = match row.get("limit") {
        None | Some(Value::Null) => 5,
        Some(limit) => limit
            .as_f64()
            .filter(|limit| limit.fract() == 0.0 && (1.0..=20.0).contains(limit))
            .ok_or("extract.limit must be an integer from 1 to 20")? as u32,
    };
    let rows = row
        .get("fields")
        .and_then(Value::as_array)
        .filter(|rows| (1..=6).contains(&rows.len()))
        .ok_or("extract.fields needs 1 to 6 fields")?;
    let mut fields: Vec<CliExtractField> = Vec::new();
    for field in rows {
        let item = field.as_object().ok_or("extract field must be an object")?;
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| FIELD_NAME_RE.is_match(name))
            .ok_or("extract field name must be a short identifier")?;
        if fields.iter().any(|seen| seen.name == name) {
            return Err(format!("duplicate extract field: {name}"));
        }
        let mut next = CliExtractField {
            name: name.to_owned(),
            selector: None,
            attr: None,
        };
        if let Some(selector) = item.get("selector").filter(|value| !value.is_null()) {
            // The length and control-character checks see the untrimmed text.
            let selector = selector
                .as_str()
                .filter(|text| {
                    !js_trim(text).is_empty() && utf16_len(text) <= 200 && !has_control(text)
                })
                .ok_or_else(|| format!("extract field {name} has a bad selector"))?;
            next.selector = Some(js_trim(selector).to_owned());
        }
        if let Some(attr) = item.get("attr").filter(|value| !value.is_null())
            && attr.as_str() != Some("text")
        {
            let attr = attr
                .as_str()
                .filter(|attr| ATTR_RE.is_match(attr))
                .ok_or_else(|| format!("extract field {name} has a bad attr"))?;
            next.attr = Some(attr.to_owned());
        }
        fields.push(next);
    }
    Ok(Some(CliExtract {
        selector: selector.to_owned(),
        limit,
        fields,
    }))
}

/// Keep a valid extract and drop a broken one. Other command fields stay as saved.
pub fn attach_extract(mut command: CliCommand) -> CliCommand {
    command.extract = command
        .extract
        .take()
        .and_then(|extract| serde_json::to_value(extract).ok())
        .and_then(|raw| parse_extract(&raw).ok().flatten());
    command
}

pub fn scrub_extract_items(
    items: &[Record<String>],
    fields: &[CliExtractField],
) -> Vec<Record<String>> {
    items
        .iter()
        .map(|item| {
            let mut out: Record<String> = Record::new();
            // The named fields come first, in the order the extract lists them. A key
            // written twice keeps its first position.
            let named = fields
                .iter()
                .filter_map(|field| item.get_key_value(&field.name));
            for (key, value) in named.chain(item) {
                let text = if secret_field(key) {
                    "****".to_owned()
                } else {
                    scrub_secrets(value)
                };
                out.insert(key.clone(), utf16_prefix(&text, FIELD_VALUE_MAX).to_owned());
            }
            js_key_order(out)
        })
        .collect()
}

fn looks_like_html(body: &str) -> bool {
    let head = utf16_prefix(body, 300)
        .trim_start_matches(is_js_space)
        .to_lowercase();
    ["<!doctype html", "<html", "<head", "<body"]
        .iter()
        .any(|tag| head.starts_with(tag))
}

/// The part of a run result `run_json_body` reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliRunBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_cut: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Record<String>>>,
}

/// The run body to show in a JSON viewer, or None when it is cut, extracted, or not JSON.
pub fn run_json_body(result: &CliRunBody) -> Option<&str> {
    // An empty item list still means the run extracted.
    if result.items.is_some() || result.body_cut == Some(true) {
        return None;
    }
    let body = non_empty(&result.body)?;
    let trimmed = js_trim(body);
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    serde_json::from_str::<IgnoredAny>(trimmed)
        .is_ok()
        .then_some(body)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentedBody {
    pub body: String,
    pub body_cut: bool,
}

/// HTML pages are previewed. JSON and other short bodies stay whole.
pub fn present_raw_body(body: &str, body_truncated: bool) -> PresentedBody {
    let whole = !looks_like_html(body) || (!body_truncated && utf16_len(body) <= RAW_PREVIEW_CHARS);
    let shown = if whole {
        body
    } else {
        utf16_prefix(body, RAW_PREVIEW_CHARS)
    };
    PresentedBody {
        body: shown.to_owned(),
        body_cut: !whole,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractSpec {
    pub url: String,
    pub method: String,
    pub headers: Record<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub extract: CliExtract,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtractJob<'a> {
    url: &'a str,
    method: &'a str,
    headers: &'a Record<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
    timeout_ms: u64,
    max_chars: usize,
    selector: &'a str,
    limit: u32,
    fields: &'a [CliExtractField],
    note_none: &'a str,
    note_capped: &'a str,
}

const EXTRACT_SCRIPT_HEAD: &str = "\n(async () => {\n  const job = ";
const EXTRACT_SCRIPT_TAIL: &str = r#"
  const ac = new AbortController()
  const timer = setTimeout(() => ac.abort(), job.timeoutMs)
  try {
    const init = {
      method: job.method,
      headers: job.headers,
      credentials: 'include',
      redirect: 'follow',
      cache: 'no-store',
      signal: ac.signal
    }
    if (job.body != null && job.method !== 'GET' && job.method !== 'HEAD') init.body = job.body
    const res = await fetch(job.url, init)
    clearTimeout(timer)
    const buf = await res.arrayBuffer()
    const bytes = new Uint8Array(buf)
    const total = bytes.length
    const slice = total > job.maxChars ? bytes.slice(0, job.maxChars) : bytes
    const ctm = /charset\s*=\s*["']?([^;"'\s]+)/i.exec(res.headers.get('content-type') || '')
    let html
    try {
      html = new TextDecoder(ctm ? ctm[1].toLowerCase() : 'utf-8', { fatal: false }).decode(slice)
    } catch (_e) {
      html = new TextDecoder('utf-8', { fatal: false }).decode(slice)
    }
    if (html.charCodeAt(0) === 0xfeff) html = html.slice(1)
    const doc = new DOMParser().parseFromString(html, 'text/html')
    let nodes
    try {
      nodes = Array.from(doc.querySelectorAll(job.selector))
    } catch (_e) {
      return { status: res.status, error: 'invalid selector', items: [], matched: 0 }
    }
    const items = []
    for (const node of nodes.slice(0, job.limit)) {
      const item = {}
      for (const field of job.fields) {
        let el = node
        if (field.selector) {
          try { el = node.querySelector(field.selector) } catch (_e) { el = null }
        }
        let value = ''
        if (el) {
          if (field.attr && field.attr !== 'text') value = el.getAttribute(field.attr) || ''
          else value = el.textContent || ''
        }
        value = value.replace(/\s+/g, ' ').trim()
        if (value.length > 500) value = value.slice(0, 500)
        if ((field.attr === 'href' || field.attr === 'src') && value) {
          try { value = new URL(value, res.url).href } catch (_e) {}
        }
        item[field.name] = value
      }
      items.push(item)
    }
    const truncated = total > job.maxChars
    let note
    if (nodes.length === 0) note = truncated ? job.noteNone + '; ' + job.noteCapped : job.noteNone
    else if (truncated) note = job.noteCapped
    return { status: res.status, matched: nodes.length, items, note }
  } catch (e) {
    clearTimeout(timer)
    const isAbort = e && e.name === 'AbortError'
    return {
      status: 0,
      items: [],
      matched: 0,
      error: isAbort
        ? 'fetch aborted after ' + job.timeoutMs + 'ms with no response'
        : ((e && e.message) ? e.message : String(e))
    }
  }
})()
"#;

/// In-page fetch plus DOMParser. Selector text is JSON, never spliced into the script.
pub fn build_extract_expression(spec: &ExtractSpec) -> String {
    let has_body = spec.method != "GET" && spec.method != "HEAD";
    let job = ExtractJob {
        url: &spec.url,
        method: &spec.method,
        headers: &spec.headers,
        body: spec.body.as_deref().filter(|_| has_body),
        timeout_ms: EXTRACT_FETCH_TIMEOUT_MS,
        max_chars: EXTRACT_MAX_CHARS,
        selector: &spec.extract.selector,
        limit: spec.extract.limit,
        fields: &spec.extract.fields,
        note_none: EXTRACT_NOTE_NONE,
        note_capped: EXTRACT_NOTE_CAPPED,
    };
    let job = serde_json::to_string(&job).unwrap_or_default();
    format!("{EXTRACT_SCRIPT_HEAD}{job}{EXTRACT_SCRIPT_TAIL}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const UUID: &str = "550e8400-e29b-41d4-a716-446655440000";
    const UUID_2: &str = "550e8400-e29b-41d4-a716-446655440001";

    fn call(request_id: &str, url: &str) -> CapturedCall {
        CapturedCall {
            request_id: request_id.to_owned(),
            method: "GET".to_owned(),
            url: url.to_owned(),
            ..CapturedCall::default()
        }
    }

    fn post(request_id: &str, url: &str, body: &str) -> CapturedCall {
        CapturedCall {
            method: "POST".to_owned(),
            request_post_data: Some(body.to_owned()),
            ..call(request_id, url)
        }
    }

    fn compile(calls: &[CapturedCall]) -> CliData {
        compile_cli(calls, &CompileOptions::default()).expect("compiles")
    }

    fn record<V, const N: usize>(pairs: [(&str, V); N]) -> Record<V> {
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect()
    }

    fn strings<const N: usize>(pairs: [(&str, &str); N]) -> Record<String> {
        record(pairs.map(|(key, value)| (key, value.to_owned())))
    }

    fn param(kind: CliParamKind, required: bool) -> CliParam {
        CliParam { kind, required }
    }

    fn optional(kind: CliParamKind) -> CliParam {
        param(kind, false)
    }

    fn field(name: &str, selector: Option<&str>, attr: Option<&str>) -> CliExtractField {
        CliExtractField {
            name: name.to_owned(),
            selector: selector.map(str::to_owned),
            attr: attr.map(str::to_owned),
        }
    }

    fn command(name: &str, method: &str, path: &str) -> CliCommand {
        CliCommand {
            name: name.to_owned(),
            desc: "d".to_owned(),
            method: method.to_owned(),
            path: path.to_owned(),
            effect: if method == "GET" {
                CliEffect::Read
            } else {
                CliEffect::Write
            },
            evidence_request_ids: vec!["r1".to_owned()],
            params: Record::new(),
            query: Record::new(),
            body: None,
            session_headers: Vec::new(),
            response_pointers: Vec::new(),
            extract: None,
        }
    }

    fn keys<V>(map: &Record<V>) -> Vec<&str> {
        map.keys().map(String::as_str).collect()
    }

    fn paths(data: &CliData) -> Vec<&str> {
        data.commands.iter().map(|c| c.path.as_str()).collect()
    }

    fn names(data: &CliData) -> Vec<&str> {
        data.commands.iter().map(|c| c.name.as_str()).collect()
    }

    /// The four-command CLI the describe, curl and request tests share.
    fn shop() -> CliData {
        let orders = CliCommand {
            evidence_request_ids: vec!["a".to_owned(), "b".to_owned()],
            params: record([("id0", param(CliParamKind::String, true))]),
            query: record([
                ("page", param(CliParamKind::Int, true)),
                ("a b", optional(CliParamKind::String)),
            ]),
            session_headers: vec!["x-csrf-token".to_owned(), "x-xsrf".to_owned()],
            ..command("get-orders-id0", "GET", "/orders/{id0}")
        };
        let items = CliCommand {
            evidence_request_ids: vec!["c".to_owned()],
            body: Some(CliBody {
                json: Some(record([
                    ("title", optional(CliParamKind::String)),
                    ("n", param(CliParamKind::Int, true)),
                ])),
                form: None,
            }),
            ..command("post-items", "POST", "/items")
        };
        let login = CliCommand {
            evidence_request_ids: vec!["d".to_owned()],
            body: Some(CliBody {
                json: None,
                form: Some(record([
                    ("user name", optional(CliParamKind::String)),
                    ("ok", optional(CliParamKind::Bool)),
                ])),
            }),
            ..command("post-login", "POST", "/login")
        };
        let search = CliCommand {
            evidence_request_ids: vec!["e".to_owned()],
            extract: Some(CliExtract {
                selector: "li.row".to_owned(),
                limit: 3,
                fields: vec![
                    field("title", Some("h2"), None),
                    field("link", Some("a"), Some("href")),
                ],
            }),
            ..command("get-search", "GET", "/search")
        };
        CliData {
            site: "shop".to_owned(),
            title: "Shop".to_owned(),
            base_url: "https://shop.example".to_owned(),
            commands: vec![orders, items, login, search],
            writes: true,
        }
    }

    // ── cli-spec.test.ts ────────────────────────────────────────────────────

    #[test]
    fn 같은_uuid_경로의_두_호출은_명령_하나로_합쳐지고_경로는_id0이_된다() {
        let data = compile(&[
            CapturedCall {
                status: Some(200),
                ..call("a", &format!("https://shop.example/api/orders/{UUID}"))
            },
            CapturedCall {
                status: Some(200),
                ..call("b", &format!("https://shop.example/api/orders/{UUID_2}"))
            },
        ]);
        assert_eq!(data.commands.len(), 1);
        assert_eq!(data.commands[0].path, "/api/orders/{id0}");
        assert_eq!(data.commands[0].evidence_request_ids, ["a", "b"]);
        assert!(!data.writes);
    }

    #[test]
    fn js와_woff2는_명령이_되지_않는다() {
        let typed = |id: &str, url: &str, resource_type: &str| CapturedCall {
            resource_type: Some(resource_type.to_owned()),
            ..call(id, url)
        };
        let data = compile(&[
            typed("js", "https://shop.example/assets/app.js", "Script"),
            typed("font", "https://shop.example/font.woff2", "Font"),
            CapturedCall {
                status: Some(200),
                ..typed("api", "https://shop.example/api/health", "XHR")
            },
        ]);
        assert_eq!(paths(&data), ["/api/health"]);
    }

    #[test]
    fn q와_json_title은_플래그가_되고_csrf_token과_authorization은_빠진다() {
        let data = compile(&[CapturedCall {
            resource_type: Some("Fetch".to_owned()),
            status: Some(201),
            mime_type: Some("application/json".to_owned()),
            request_headers: Some(strings([
                ("authorization", "Bearer secret-value"),
                ("x-csrf-token", "secret-value"),
            ])),
            response_body: Some(r#"{"ok":true}"#.to_owned()),
            ..post(
                "post",
                "https://shop.example/api/items?q=hat",
                r#"{"title":"Hat","csrf_token":"secret-value","authorization":"secret-value","pass":"hunter2"}"#,
            )
        }]);
        let command = &data.commands[0];
        let flag = Some(&optional(CliParamKind::String));
        assert_eq!(command.query.get("q"), flag);
        let json = json_fields(command).expect("json body");
        assert_eq!(json.get("title"), flag);
        assert_eq!(json.get("pass"), flag);
        assert_eq!(json.get("csrf_token"), None);
        assert_eq!(json.get("authorization"), None);
        assert_eq!(command.session_headers, ["x-csrf-token"]);
        let exported = export_curl(&data);
        assert!(exported.contains("$title"));
        assert!(exported.contains("$pass"));
        assert!(!exported.contains("hunter2"));
        assert!(!exported.contains("secret-value"));
        assert!(!exported.contains("Cookie"));
        let saved = serde_json::to_string(&data).expect("serializes");
        assert!(!saved.contains("hunter2"));
        assert!(!saved.contains("secret-value"));
    }

    #[test]
    fn get_users_delete는_write다() {
        let data = compile(&[CapturedCall {
            status: Some(200),
            ..call("del", "https://shop.example/users/delete")
        }]);
        assert_eq!(data.commands[0].effect, CliEffect::Write);
    }

    #[test]
    fn names로_request_id별_명령_이름을_지정한다() {
        let opts = CompileOptions {
            names: Some(strings([("s1", "Search Titles!")])),
            ..CompileOptions::default()
        };
        let calls = [call(
            "s1",
            "https://en.wikipedia.org/w/rest.php/v1/search/title?q=x&limit=10",
        )];
        let data = compile_cli(&calls, &opts).expect("compiles");
        assert_eq!(data.commands[0].name, "search-titles");
    }

    #[test]
    fn 지정한_이름은_effect_판정을_바꾸지_않는다() {
        let opts = CompileOptions {
            names: Some(strings([("del", "search")])),
            ..CompileOptions::default()
        };
        let calls = [CapturedCall {
            status: Some(200),
            ..call("del", "https://shop.example/users/delete")
        }];
        let data = compile_cli(&calls, &opts).expect("compiles");
        assert_eq!(data.commands[0].name, "search");
        assert_eq!(data.commands[0].effect, CliEffect::Write);
    }

    #[test]
    fn 지정한_이름이_겹치거나_비면_자동_이름_규칙을_따른다() {
        let opts = CompileOptions {
            names: Some(strings([("a", "list"), ("b", "list"), ("c", "!!!")])),
            ..CompileOptions::default()
        };
        let calls = [
            call("a", "https://shop.example/api/items"),
            call("b", "https://shop.example/api/orders"),
            call("c", "https://shop.example/api/cart"),
        ];
        let data = compile_cli(&calls, &opts).expect("compiles");
        assert_eq!(names(&data), ["list", "list-x", "get-api-cart"]);
    }

    #[test]
    fn curl_내보내기는_클라이언트를_밝히는_user_agent_자리를_둔다() {
        let data = compile(&[call(
            "s1",
            "https://en.wikipedia.org/w/rest.php/v1/search/title?q=x",
        )]);
        let exported = export_curl(&data);
        assert!(exported.lines().any(|line| {
            line.strip_prefix("# UA ")
                .is_some_and(|rest| rest.contains("User-Agent"))
        }));
        assert!(exported.contains("curl -sS -A \"$UA\" -X GET"));
    }

    fn run_body(body: &str) -> CliRunBody {
        CliRunBody {
            status: Some(200),
            body: Some(body.to_owned()),
            ..CliRunBody::default()
        }
    }

    #[test]
    fn 실행_결과가_온전한_json_본문이면_뷰어에_넘길_본문을_돌려준다() {
        let body = r#"{"pages":[{"title":"Web browser"}]}"#;
        assert_eq!(run_json_body(&run_body(body)), Some(body));
        assert_eq!(run_json_body(&run_body(" [1,2] ")), Some(" [1,2] "));
    }

    #[test]
    fn 잘린_본문_html_추출_결과는_json_뷰어로_보내지_않는다() {
        let cut = CliRunBody {
            body_cut: Some(true),
            ..run_body(r#"{"pages":["#)
        };
        assert_eq!(run_json_body(&cut), None);
        assert_eq!(
            run_json_body(&run_body("<html><body>hi</body></html>")),
            None
        );
        let extracted = CliRunBody {
            items: Some(vec![strings([("title", "x")])]),
            ..run_body(r#"{"a":1}"#)
        };
        assert_eq!(run_json_body(&extracted), None);
        assert_eq!(run_json_body(&run_body("{not json}")), None);
        let no_body = CliRunBody {
            status: Some(200),
            ..CliRunBody::default()
        };
        assert_eq!(run_json_body(&no_body), None);
    }

    #[test]
    fn 빈_request_id는_증거가_되지_않는다() {
        let data = compile(&[
            call("", "https://shop.example/api/health"),
            call("   ", "https://shop.example/api/health"),
        ]);
        assert_eq!(data.commands, []);
    }

    #[test]
    fn writes가_꺼져_있으면_write_실행은_요청을_만들지_않고_막는다() {
        assert_eq!(run_gate(CliEffect::Write, false), Some(WRITES_DISABLED));
        assert_eq!(run_gate(CliEffect::Unknown, false), Some(WRITES_DISABLED));
        assert_eq!(run_gate(CliEffect::Read, false), None);
        assert_eq!(run_gate(CliEffect::Write, true), None);
    }

    #[test]
    fn extract는_셀렉터와_필드로_저장되고_한계는_1에서_20이다() {
        let parsed = parse_extract(&json!({
            "selector": "a[data-heatmap-target=\".nblg\"]",
            "fields": [
                { "name": "title", "selector": ".headline" },
                { "name": "link", "attr": "href" },
                { "name": "label", "attr": "text" }
            ]
        }));
        let extract = parsed.expect("ok").expect("extract");
        assert_eq!(extract.limit, 5);
        assert_eq!(extract.fields[1], field("link", None, Some("href")));
        assert_eq!(extract.fields[2], field("label", None, None));
        let title = json!([{ "name": "title" }]);
        assert!(parse_extract(&json!({ "selector": "a", "limit": 20, "fields": title })).is_ok());
        assert!(parse_extract(&json!({ "selector": "a", "limit": 21, "fields": title })).is_err());
        assert!(parse_extract(&json!({ "selector": "a", "limit": 0, "fields": title })).is_err());
        assert!(parse_extract(&json!({ "fields": title })).is_err());
        assert!(parse_extract(&json!({ "selector": "a\nb", "fields": title })).is_err());
        let twice = json!([{ "name": "title" }, { "name": "title" }]);
        assert!(parse_extract(&json!({ "selector": "a", "fields": twice })).is_err());
    }

    #[test]
    fn 깨진_extract는_명령에서_빠지고_설명과_curl에는_셀렉터만_남는다() {
        let command = CliCommand {
            desc: "search".to_owned(),
            query: record([("query", optional(CliParamKind::String))]),
            extract: Some(CliExtract {
                selector: "article h1".to_owned(),
                limit: 5,
                fields: vec![
                    field("title", None, None),
                    field("link", None, Some("href")),
                ],
            }),
            ..command("get-search", "GET", "/search")
        };
        let data = CliData {
            site: "example".to_owned(),
            title: "example".to_owned(),
            base_url: "https://example.test".to_owned(),
            commands: vec![command.clone()],
            writes: false,
        };
        let described = describe_cli(&data, Some("get-search"));
        assert!(described.contains("extract: article h1  limit 5"));
        assert!(described.contains("title text"));
        assert!(described.contains("link attr href"));
        let exported = export_curl(&data);
        assert!(exported.contains("# selector: article h1"));
        assert!(exported.contains("# reads 5: title, link"));
        assert!(!exported.contains("Cookie"));
        let broken = CliCommand {
            extract: Some(CliExtract {
                selector: String::new(),
                limit: 5,
                fields: Vec::new(),
            }),
            ..command.clone()
        };
        assert_eq!(attach_extract(broken).extract, None);
        let kept = attach_extract(command).extract;
        assert_eq!(
            kept.map(|extract| extract.selector).as_deref(),
            Some("article h1")
        );
    }

    #[test]
    fn 호출_예시는_id_명령_플래그만_담고_캡처_값에서_시크릿은_빼낸다() {
        let command = CliCommand {
            desc: "search".to_owned(),
            query: record(
                ["ssc", "where", "query", "token"]
                    .map(|name| (name, optional(CliParamKind::String))),
            ),
            ..command("get-searchnaver", "GET", "/search.naver")
        };
        let captured = captured_flag_values(
            &command,
            &[CapturedFlagSource {
                url: "https://search.naver.com/search.naver?ssc=tab.blog.all&where=blog&query=%EC%A0%9C%EC%A3%BC%EB%8F%84+%EB%A7%9B%EC%A7%91&token=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxIn0.signaturevalue".to_owned(),
                request_post_data: None,
                request_headers: Some(strings([("cookie", "NID_SES=secret")])),
            }],
        );
        assert_eq!(
            captured,
            strings([
                ("ssc", "tab.blog.all"),
                ("where", "blog"),
                ("query", "제주도 맛집")
            ])
        );
        assert!(!captured.contains_key("token"));
        let example = cli_run_example("wmvew9vxip06nak", &command, &captured);
        let parsed: Value = serde_json::from_str(&example).expect("json");
        assert_eq!(
            parsed,
            json!({
                "id": "wmvew9vxip06nak",
                "command": "get-searchnaver",
                "inputs": { "ssc": "tab.blog.all", "where": "blog", "query": "제주도 맛집", "token": "" }
            })
        );
        assert!(!parsed.to_string().to_lowercase().contains("cookie"));
        let blank = cli_run_example("wmvew9vxip06nak", &command, &Record::new());
        let blank: Value = serde_json::from_str(&blank).expect("json");
        assert_eq!(blank["inputs"]["query"], "");
    }

    #[test]
    fn html_본문은_앞부분만_남기고_json_본문은_그대로_둔다() {
        let html = format!(
            "<!doctype html><html><body>{}</body></html>",
            "x".repeat(RAW_PREVIEW_CHARS)
        );
        let cut = present_raw_body(&html, true);
        assert!(cut.body_cut);
        assert_eq!(utf16_len(&cut.body), RAW_PREVIEW_CHARS);
        let json = r#"{"name":"rever"}"#;
        assert_eq!(
            present_raw_body(json, false),
            PresentedBody {
                body: json.to_owned(),
                body_cut: false
            }
        );
        let long = format!("{json}{}", "y".repeat(2_000));
        assert!(!present_raw_body(&long, false).body_cut);
    }

    #[test]
    fn extract_값의_시크릿은_지우고_셀렉터는_스크립트에_문자열로만_들어간다() {
        let items = scrub_extract_items(
            &[
                strings([
                    ("title", "Bearer abcdefghijklmnop"),
                    ("token", "visible-token"),
                ]),
                strings([("link", "https://blog.example/1")]),
            ],
            &[],
        );
        assert_eq!(items[0]["title"], "Bearer ****");
        assert_eq!(items[0]["token"], "****");
        assert_eq!(items[1]["link"], "https://blog.example/1");
        let ordered = scrub_extract_items(
            &[strings([
                ("link", "https://blog.example/1"),
                ("title", "제목"),
            ])],
            &[
                field("title", None, None),
                field("link", None, Some("href")),
            ],
        );
        assert_eq!(keys(&ordered[0]), ["title", "link"]);
        let selector = r#"a[href="x"]</script>"#;
        let expr = build_extract_expression(&ExtractSpec {
            url: "https://search.example/search".to_owned(),
            method: "GET".to_owned(),
            headers: Record::new(),
            body: None,
            extract: CliExtract {
                selector: selector.to_owned(),
                limit: 5,
                fields: vec![
                    field("title", Some(".h"), None),
                    field("link", None, Some("href")),
                ],
            },
        });
        assert!(expr.contains(&serde_json::to_string(selector).expect("json")));
        assert!(expr.contains("credentials: 'include'"));
        assert!(expr.contains(EXTRACT_NOTE_NONE));
        assert!(!expr.contains("Cookie"));
        assert!(!expr.contains("eval("));
    }

    // ── Not in the TypeScript tests. Expected values were read off the TypeScript. ──

    #[test]
    fn 위험한_단어는_camel_case와_퍼센트_인코딩_뒤에서도_찾는다() {
        for risky in [
            "deleteUser",
            "%64elete",
            "x1Add",
            "logOut",
            "markAsRead",
            "MARKREAD",
            "blog-outline",
        ] {
            assert!(dangerous_intent(risky), "{risky}");
        }
        // A broken escape leaves the text undecoded, "+" included.
        for harmless in ["getUsers", "address", "sign+out", "%zzdelete", ""] {
            assert!(!dangerous_intent(harmless), "{harmless}");
        }
    }

    #[test]
    fn 시크릿_필드는_구분자를_빼고_끝말로_판정한다() {
        for secret in [
            "X-CSRF-Token",
            "oauth",
            "session_id",
            "API-KEY",
            "csrf_token",
        ] {
            assert!(secret_field(secret), "{secret}");
        }
        for plain in ["pass", "sessions", "secretary", "q", ""] {
            assert!(!secret_field(plain), "{plain}");
        }
    }

    #[test]
    fn 정적_자산은_확장자_대소문자를_가리지_않는다() {
        assert!(static_asset("/a.JS"));
        assert!(static_asset("/x.map"));
        assert!(!static_asset("/a.json"));
        assert!(!static_asset("/x.jsx"));
    }

    #[test]
    fn bearer와_jwt는_자바스크립트의_단어_경계와_공백_규칙으로_가린다() {
        assert_eq!(scrub_secrets("Bearer abcdefgh12== x"), "Bearer **** x");
        // "é" is not a word character in JavaScript, so a boundary follows it.
        assert_eq!(scrub_secrets("éBearer abcdefgh"), "éBearer ****");
        assert_eq!(scrub_secrets("xBearer abcdefgh"), "xBearer abcdefgh");
        assert_eq!(scrub_secrets("Bearer abcdefg"), "Bearer abcdefg");
        assert_eq!(scrub_secrets("Bearer\u{feff}abcdefgh"), "Bearer ****");
        assert_eq!(
            scrub_secrets("Bearer\u{85}abcdefgh"),
            "Bearer\u{85}abcdefgh"
        );
        assert_eq!(
            scrub_secrets("eyJhbGciOiJIUzI1NiJ9.abc.def tail"),
            "**** tail"
        );
    }

    #[test]
    fn 명령_수가_limit을_넘으면_json_응답을_먼저_남기고_범위_밖_limit은_20이다() {
        let limited = |calls: &[CapturedCall], limit: i64| {
            let opts = CompileOptions {
                limit: Some(limit),
                ..CompileOptions::default()
            };
            compile_cli(calls, &opts).expect("compiles")
        };
        let mixed = [
            call("1", "https://t.example/p1"),
            call("2", "https://t.example/p2"),
            CapturedCall {
                mime_type: Some("Application/JSON".to_owned()),
                ..call("3", "https://t.example/p3")
            },
            CapturedCall {
                response_body: Some("\u{feff} [1]".to_owned()),
                ..call("4", "https://t.example/p4")
            },
        ];
        assert_eq!(paths(&limited(&mixed, 2)), ["/p3", "/p4"]);
        assert_eq!(paths(&limited(&mixed, 4)), ["/p1", "/p2", "/p3", "/p4"]);
        let many: Vec<CapturedCall> = (0..25)
            .map(|i| call(&format!("r{i}"), &format!("https://t.example/p{i}")))
            .collect();
        assert_eq!(compile(&many).commands.len(), 20);
        assert_eq!(limited(&many, 0).commands.len(), 20);
        assert_eq!(limited(&many, 101).commands.len(), 20);
        assert_eq!(limited(&many, 100).commands.len(), 25);
    }

    #[test]
    fn base_url은_다른_출처를_거르고_site와_title의_기본값이_된다() {
        let calls = [
            call("1", "https://a.example/x"),
            CapturedCall {
                status: Some(500),
                ..call("2", "https://b.example:8443/y")
            },
        ];
        let opts = CompileOptions {
            base_url: Some("https://B.example:8443/some/path?x=1".to_owned()),
            ..CompileOptions::default()
        };
        let data = compile_cli(&calls, &opts).expect("compiles");
        assert_eq!(data.site, "b-example-8443");
        assert_eq!(data.title, "b-example-8443");
        assert_eq!(data.base_url, "https://b.example:8443");
        assert_eq!(paths(&data), ["/y"]);
        assert_eq!(
            data.commands[0].desc,
            "Observed GET 1 time(s), last status 500."
        );

        let named = CompileOptions {
            site: Some("My Site!!".to_owned()),
            title: Some("Shop".to_owned()),
            ..CompileOptions::default()
        };
        let data = compile_cli(&calls, &named).expect("compiles");
        assert_eq!(
            (data.site.as_str(), data.title.as_str()),
            ("my-site", "Shop")
        );
        assert_eq!(data.base_url, "https://a.example");
        assert_eq!(paths(&data), ["/x", "/y"]);

        let empty = compile(&[]);
        assert_eq!((empty.site.as_str(), empty.base_url.as_str()), ("site", ""));
    }

    #[test]
    fn 잘못된_base_url이나_출처_없는_url은_에러다() {
        let calls = [call("1", "https://a.example/x")];
        let bad = CompileOptions {
            base_url: Some("nope".to_owned()),
            ..CompileOptions::default()
        };
        assert_eq!(compile_cli(&calls, &bad), Err("Invalid URL".to_owned()));
        // A data: URL has the origin "null", which the site name can't be read from.
        let data_url = [call("d", "data:application/json,{\"a\":1}")];
        assert_eq!(
            compile_cli(&data_url, &CompileOptions::default()),
            Err("Invalid URL".to_owned())
        );
        let named = CompileOptions {
            site: Some("data".to_owned()),
            ..CompileOptions::default()
        };
        let data = compile_cli(&data_url, &named).expect("compiles");
        assert_eq!(data.base_url, "null");
        // A call whose URL does not parse is skipped, not an error.
        assert_eq!(
            compile(&[call("x", "not a url"), call("y", "/relative")]).commands,
            []
        );
    }

    #[test]
    fn 숫자_uuid_긴_hex_세그먼트만_id_자리가_된다() {
        let calls: Vec<CapturedCall> = [
            "/u/123",
            "/u/123abc",
            "/u/0123456789abcde",
            "/u/0123456789ABCDEF",
            "/u/550E8400-E29B-41D4-A716-446655440000",
            "/a/1/b/2",
            "/",
        ]
        .iter()
        .enumerate()
        .map(|(i, path)| call(&format!("p{i}"), &format!("https://t.example{path}")))
        .collect();
        let data = compile(&calls);
        assert_eq!(
            names(&data),
            [
                "get-u-id0",
                "get-u-123abc",
                "get-u-0123456789abcde",
                "get-a-id0-b-id1",
                "get-root"
            ]
        );
        assert_eq!(
            paths(&data),
            [
                "/u/{id0}",
                "/u/123abc",
                "/u/0123456789abcde",
                "/a/{id0}/b/{id1}",
                "/"
            ]
        );
        assert_eq!(data.commands[0].evidence_request_ids, ["p0", "p3", "p4"]);
        assert_eq!(
            data.commands[3].params,
            record(["id0", "id1"].map(|name| (name, param(CliParamKind::String, true))))
        );
    }

    #[test]
    fn 쿼리와_본문_값으로_플래그_종류를_정하고_숫자_키는_앞에_둔다() {
        let data = compile(&[post(
            "k",
            "https://t.example/k?n=-5&b=true&s=x&d=&n=dup&q=1&10=x&2=y&01=z",
            r#"{"i":5.0,"big":1e21,"f":1.5,"t":true,"st":"true","nul":null,"o":{},"k":1e3,"1":"x","0":"y"}"#,
        )]);
        let command = &data.commands[0];
        // A JavaScript object lists array-index keys first, ascending; "01" is not one.
        assert_eq!(
            keys(&command.query),
            ["2", "10", "n", "b", "s", "d", "q", "01"]
        );
        let kind = |fields: &Record<CliParam>, name: &str| fields.get(name).map(|p| p.kind);
        assert_eq!(kind(&command.query, "n"), Some(CliParamKind::Int));
        assert_eq!(kind(&command.query, "b"), Some(CliParamKind::Bool));
        assert_eq!(kind(&command.query, "d"), Some(CliParamKind::String));
        let json = json_fields(command).expect("json body");
        assert_eq!(
            keys(json),
            ["0", "1", "i", "big", "f", "t", "st", "nul", "o", "k"]
        );
        // String(5.0) is "5" and String(1e3) is "1000", but String(1e21) is "1e+21".
        assert_eq!(kind(json, "i"), Some(CliParamKind::Int));
        assert_eq!(kind(json, "k"), Some(CliParamKind::Int));
        assert_eq!(kind(json, "big"), Some(CliParamKind::String));
        assert_eq!(kind(json, "f"), Some(CliParamKind::String));
        assert_eq!(kind(json, "t"), Some(CliParamKind::Bool));
        assert_eq!(kind(json, "st"), Some(CliParamKind::Bool));
        assert_eq!(kind(json, "nul"), Some(CliParamKind::String));
        assert_eq!(kind(json, "o"), Some(CliParamKind::String));
        assert!(json.values().all(|param| !param.required));
    }

    #[test]
    fn 폼_본문은_content_type이나_등호로_알아본다() {
        let body_of = |call: CapturedCall| compile(&[call]).commands.remove(0).body;
        let form = |pairs| {
            Some(CliBody {
                json: None,
                form: Some(record(pairs)),
            })
        };
        assert_eq!(
            body_of(post("f", "https://t.example/f", "a=1&b=x")),
            form([
                ("a", optional(CliParamKind::Int)),
                ("b", optional(CliParamKind::String))
            ])
        );
        // URLSearchParams skips one leading "?".
        assert_eq!(
            body_of(post("f", "https://t.example/f", "?x=1")),
            Some(CliBody {
                json: None,
                form: Some(record([("x", optional(CliParamKind::Int))]))
            })
        );
        assert_eq!(body_of(post("f", "https://t.example/f", "{bad")), None);
        assert_eq!(body_of(post("f", "https://t.example/f", "[1,2]")), None);
        let declared = CapturedCall {
            request_headers: Some(strings([(
                "Content-Type",
                "application/x-www-form-urlencoded; charset=UTF-8",
            )])),
            ..post("f", "https://t.example/f", r#"{"a":1}"#)
        };
        assert_eq!(
            body_of(declared),
            Some(CliBody {
                json: None,
                form: Some(record([(r#"{"a":1}"#, optional(CliParamKind::String))])),
            })
        );
    }

    #[test]
    fn 세션_헤더는_csrf_xsrf_이름만_소문자로_정렬해_남긴다() {
        let longest = format!("csrf-{}", "a".repeat(75));
        let too_long = format!("csrf-{}", "a".repeat(76));
        let data = compile(&[CapturedCall {
            request_headers: Some(strings([
                ("X-XSRF", "v"),
                ("X-CSRF-Token", "v"),
                ("Cookie", "c"),
                ("X_CSRF", "v"),
                (&longest, "v"),
                (&too_long, "v"),
                ("Accept", "*/*"),
            ])),
            ..call("h", "https://t.example/h")
        }]);
        assert_eq!(
            data.commands[0].session_headers,
            [&longest, "x-csrf-token", "x-xsrf"]
        );
    }

    #[test]
    fn 응답_포인터는_시크릿을_빼고_12개까지_가장_많은_응답에서_가져온다() {
        let wide: String = (0..15).map(|i| format!(r#","k{i}":{i}"#)).collect();
        let bodies = [
            r#"{"only":1}"#.to_owned(),
            format!(r#"{{"token":"t","7":1{wide}}}"#),
            r#"[{"a":1}]"#.to_owned(),
        ];
        let calls: Vec<CapturedCall> = bodies
            .into_iter()
            .enumerate()
            .map(|(i, body)| CapturedCall {
                response_body: Some(body),
                ..call(&format!("r{i}"), "https://t.example/r")
            })
            .collect();
        let pointers = &compile(&calls).commands[0].response_pointers;
        assert_eq!(
            pointers,
            &[
                "/7", "/k0", "/k1", "/k2", "/k3", "/k4", "/k5", "/k6", "/k7", "/k8", "/k9", "/k10"
            ]
        );
    }

    #[test]
    fn effect는_메서드와_mutation_단어로_판정한다() {
        let judged = |method: &str, path: &str| {
            let mut data = compile(&[CapturedCall {
                method: method.to_owned(),
                ..call("e", &format!("https://t.example{path}"))
            }]);
            let command = data.commands.remove(0);
            (command.effect, command.name)
        };
        assert_eq!(judged("PUT", "/x"), (CliEffect::Write, "put-x".to_owned()));
        assert_eq!(
            judged("POST", "/x"),
            (CliEffect::Write, "post-x".to_owned())
        );
        assert_eq!(judged("HEAD", "/x"), (CliEffect::Read, "head-x".to_owned()));
        assert_eq!(
            judged("TRACE", "/x"),
            (CliEffect::Unknown, "trace-x".to_owned())
        );
        assert_eq!(judged("get", "/x"), (CliEffect::Read, "get-x".to_owned()));
        // An empty method counts as GET.
        assert_eq!(judged("", "/x"), (CliEffect::Read, "get-x".to_owned()));
        assert_eq!(judged("GET", "/x?query=mutation+x").0, CliEffect::Write);
        assert_eq!(judged("GET", "/x?op=mutations").0, CliEffect::Read);
    }

    #[test]
    fn 상태가_없는_호출은_마지막_상태를_바꾸지_않는다() {
        let with = |id: &str, status| CapturedCall {
            status,
            ..call(id, "https://t.example/s")
        };
        let data = compile(&[
            with("s", Some(200)),
            with("s2", None),
            with("s3", Some(404)),
            with("s4", None),
        ]);
        assert_eq!(
            data.commands[0].desc,
            "Observed GET 4 time(s), last status 404."
        );
    }

    #[test]
    fn 이름은_값이_있는_첫_request_id에서_오고_40자에서_잘린다() {
        let opts = CompileOptions {
            names: Some(strings([
                ("n1", ""),
                ("n2", "Second ID wins"),
                ("n3", &format!("{} y z", "x".repeat(39))),
            ])),
            ..CompileOptions::default()
        };
        let calls = [
            call("n1", "https://t.example/a"),
            call("n2", "https://t.example/a"),
            call("n3", "https://t.example/b"),
            call("n4", "https://t.example/a/b"),
            call("n5", "https://t.example/a-b"),
        ];
        let data = compile_cli(&calls, &opts).expect("compiles");
        // The cut leaves a trailing dash, which is dropped. Two paths that slug
        // alike get "-x".
        assert_eq!(
            names(&data),
            [
                "second-id-wins",
                "x".repeat(39).as_str(),
                "get-a-b",
                "get-a-b-x"
            ]
        );
    }

    #[test]
    fn 직렬화_필드_이름은_타입스크립트와_같다() {
        let captured: CapturedCall = serde_json::from_value(json!({
            "requestId": "post",
            "method": "POST",
            "url": "https://shop.example/api/items?q=hat",
            "resourceType": "Fetch",
            "status": 201,
            "mimeType": "application/json",
            "requestHeaders": { "X-CSRF-Token": "v" },
            "requestPostData": "{\"title\":\"Hat\"}",
            "responseBody": "{\"ok\":true}"
        }))
        .expect("camelCase call");
        let data = compile(&[captured]);
        let saved = serde_json::to_string(&data).expect("serializes");
        assert_eq!(
            saved,
            r#"{"site":"shop-example","title":"shop-example","baseUrl":"https://shop.example","commands":[{"name":"post-api-items","desc":"Observed POST 1 time(s), last status 201.","method":"POST","path":"/api/items","effect":"write","evidenceRequestIds":["post"],"params":{},"query":{"q":{"kind":"string","required":false}},"body":{"json":{"title":{"kind":"string","required":false}}},"sessionHeaders":["x-csrf-token"],"responsePointers":["/ok"]}],"writes":false}"#
        );
        assert_eq!(
            serde_json::from_str::<CliData>(&saved).expect("loads"),
            data
        );
    }

    #[test]
    fn 경로의_캐럿은_url_표준처럼_퍼센트_인코딩한다() {
        let data = compile(&[call("u", "https://h.example/a^b/c")]);
        assert_eq!(data.commands[0].path, "/a%5Eb/c");
        assert_eq!(data.commands[0].name, "get-a5eb-c");
        // An opaque path keeps it.
        let named = CompileOptions {
            site: Some("s".to_owned()),
            ..CompileOptions::default()
        };
        let opaque = compile_cli(&[call("u", "foo:a^b")], &named).expect("compiles");
        assert_eq!(opaque.commands[0].path, "a^b");

        let by_id = CliCommand {
            params: record([("id0", param(CliParamKind::String, true))]),
            ..command("cmd", "GET", "/a^b/{id0}?k=1^2")
        };
        assert_eq!(
            build_cli_request("https://h.example", &by_id, &strings([("id0", "v^w")])),
            built("https://h.example/a%5Eb/v%5Ew?k=1^2", "GET", None)
        );
        // The raw segment a broken escape falls back to is the encoded one.
        let item = CliCommand {
            path: "/x/{id0}".to_owned(),
            ..by_id
        };
        let source = CapturedFlagSource {
            url: "https://h.example/x/a^%zz".to_owned(),
            ..CapturedFlagSource::default()
        };
        assert_eq!(
            captured_flag_values(&item, &[source]),
            strings([("id0", "a%5E%zz")])
        );
    }

    #[test]
    fn 저장된_명령의_플래그는_읽을_때_자바스크립트_키_순서가_된다() {
        let saved: CliCommand = serde_json::from_str(
            r#"{"name":"c","desc":"d","method":"POST","path":"/x","effect":"write","evidenceRequestIds":["r"],"params":{},"query":{"b":{"kind":"string","required":false},"10":{"kind":"int","required":false},"2":{"kind":"int","required":true}},"body":{"json":{"z":{"kind":"string","required":false},"1":{"kind":"int","required":false}}},"sessionHeaders":[],"responsePointers":[]}"#,
        )
        .expect("loads");
        let data = CliData {
            site: "s".to_owned(),
            title: "t".to_owned(),
            base_url: "https://h.example".to_owned(),
            commands: vec![saved],
            writes: false,
        };
        assert_eq!(
            describe_cli(&data, None),
            "s  https://h.example\nwrites: off\n\nc\n  POST /x\n  effect: write\n  --2 int required query\n  --10 int query\n  --b string query\n  --1 int json\n  --z string json\n  evidence: r\n"
        );
        // The first required flag without a value is the one reported.
        assert_eq!(
            build_cli_request(
                "https://h.example",
                &data.commands[0],
                &strings([("b", "x"), ("z", "y"), ("1", "5")])
            ),
            Err("missing 2".to_owned())
        );
    }

    #[test]
    fn 저장된_명령의_깨진_extract는_읽을_때_빠진다() {
        let saved = |extract: Value| {
            let mut raw =
                serde_json::to_value(command("get-search", "GET", "/search")).expect("value");
            raw["extract"] = extract;
            serde_json::from_value::<CliCommand>(raw).expect("loads")
        };
        assert_eq!(
            saved(json!({ "selector": "", "limit": 5, "fields": [] })).extract,
            None
        );
        assert_eq!(saved(json!("li")).extract, None);
        let kept =
            saved(json!({ "selector": " li ", "fields": [{ "name": "t", "attr": "text" }] }));
        assert_eq!(
            kept.extract,
            Some(CliExtract {
                selector: "li".to_owned(),
                limit: 5,
                fields: vec![field("t", None, None)]
            })
        );
        assert_eq!(kept.name, "get-search");
    }

    #[test]
    fn describe는_플래그와_extract를_적고_없는_명령이면_이름을_알려준다() {
        let data = shop();
        let all = "shop  https://shop.example\nwrites: on\n\n\
get-orders-id0\n  GET /orders/{id0}\n  effect: read\n  --id0 string required path\n  --page int required query\n  --a b string query\n  session headers: x-csrf-token, x-xsrf\n  evidence: a, b\n\n\
post-items\n  POST /items\n  effect: write\n  --title string json\n  --n int required json\n  evidence: c\n\n\
post-login\n  POST /login\n  effect: write\n  --user name string form\n  --ok bool form\n  evidence: d\n\n\
get-search\n  GET /search\n  effect: read\n  extract: li.row  limit 3\n    title text h2\n    link attr href a\n  evidence: e\n";
        assert_eq!(describe_cli(&data, None), all);
        // An empty name means every command, as in the TypeScript's truthiness check.
        assert_eq!(describe_cli(&data, Some("")), all);
        assert_eq!(
            describe_cli(&data, Some("nope")),
            "shop  https://shop.example\nwrites: on\n\nUnknown action: nope\nAvailable actions: get-orders-id0, post-items, post-login, get-search"
        );
    }

    #[test]
    fn curl_내보내기는_경로_쿼리_본문_자리를_만든다() {
        let expected = [
            "# shop https://shop.example",
            "# writes: on",
            "# BASE is the site origin. Flags are placeholders. Captured secrets are not included.",
            "# UA is your client's User-Agent, e.g. \"my-tool/1.0 (you@example.com)\". Some APIs require one.",
            "",
            "# get-orders-id0  GET /orders/{id0}  read",
            "curl -sS -A \"$UA\" -X GET \"$BASE/orders/$id0?page=$page&a%20b=$a b\"",
            "",
            "# post-items  POST /items  write",
            "curl -sS -A \"$UA\" -X POST \"$BASE/items\" -H 'content-type: application/json' --data '{\"title\":\"$title\",\"n\":\"$n\"}'",
            "",
            "# post-login  POST /login  write",
            "curl -sS -A \"$UA\" -X POST \"$BASE/login\" --data 'user%20name=$user name&ok=$ok'",
            "",
            "# get-search  GET /search  read",
            "# reads 3: title, link",
            "# selector: li.row",
            "curl -sS -A \"$UA\" -X GET \"$BASE/search\"",
            "",
        ];
        assert_eq!(export_curl(&shop()), expected.join("\n"));
    }

    fn built(url: &str, method: &str, body: Option<&str>) -> Result<BuiltCliRequest, String> {
        Ok(BuiltCliRequest {
            url: url.to_owned(),
            method: method.to_owned(),
            body: body.map(str::to_owned),
        })
    }

    #[test]
    fn 요청은_경로_쿼리_본문을_채워_만든다() {
        let data = shop();
        let (orders, items, login) = (&data.commands[0], &data.commands[1], &data.commands[2]);
        assert_eq!(
            build_cli_request(
                "https://shop.example",
                orders,
                &strings([("id0", "a b/é"), ("page", "2"), ("a b", "x y&z")])
            ),
            built(
                "https://shop.example/orders/a%20b%2F%C3%A9?page=2&a+b=x+y%26z",
                "GET",
                None
            )
        );
        assert_eq!(
            build_cli_request(
                "https://shop.example",
                items,
                &strings([("title", "Hat \"L\""), ("n", "2")])
            ),
            built(
                "https://shop.example/items",
                "POST",
                Some(r#"{"title":"Hat \"L\"","n":"2"}"#)
            )
        );
        assert_eq!(
            build_cli_request(
                "https://shop.example/",
                login,
                &strings([("user name", "a b&c"), ("ok", "true")])
            ),
            built(
                "https://shop.example/login",
                "POST",
                Some("user+name=a+b%26c&ok=true")
            )
        );
        // No field has a value, so there is no body.
        assert_eq!(
            build_cli_request("https://shop.example", login, &Record::new()),
            built("https://shop.example/login", "POST", None)
        );
        // The path replaces the base URL's own path and query.
        let flat = CliCommand {
            params: Record::new(),
            path: "/x".to_owned(),
            ..orders.clone()
        };
        assert_eq!(
            build_cli_request(
                "https://shop.example/api/v1?keep=1",
                &flat,
                &strings([("page", "1")])
            ),
            built("https://shop.example/x?page=1", "GET", None)
        );
    }

    #[test]
    fn 필수_플래그가_없거나_base_url이_잘못되면_요청을_만들지_않는다() {
        let data = shop();
        let (orders, items) = (&data.commands[0], &data.commands[1]);
        let run = |base: &str, command: &CliCommand, inputs| {
            build_cli_request(base, command, &strings(inputs))
        };
        assert_eq!(
            run("https://shop.example", orders, [("page", "2"), ("x", "")]),
            Err("missing id0".to_owned())
        );
        // An empty value is a missing one.
        assert_eq!(
            run("https://shop.example", orders, [("id0", ""), ("page", "2")]),
            Err("missing id0".to_owned())
        );
        assert_eq!(
            run("https://shop.example", orders, [("id0", "7"), ("x", "")]),
            Err("missing page".to_owned())
        );
        assert_eq!(
            run("https://shop.example", items, [("title", "Hat"), ("x", "")]),
            Err("missing n".to_owned())
        );
        assert_eq!(
            run("nope", orders, [("id0", "7"), ("page", "2")]),
            Err("Invalid URL".to_owned())
        );
        assert_eq!(
            run("", orders, [("id0", "7"), ("page", "2")]),
            Err("Invalid URL".to_owned())
        );
    }

    #[test]
    fn 이미_있는_쿼리_이름은_첫_값을_바꾸고_나머지는_지운다() {
        let orders = shop().commands.remove(0);
        let command = CliCommand {
            params: Record::new(),
            path: "/p?page=9&z=%zz&page=8".to_owned(),
            ..orders
        };
        assert_eq!(
            build_cli_request(
                "https://shop.example",
                &command,
                &strings([("page", "2"), ("a b", "new")])
            ),
            built("https://shop.example/p?page=2&z=%25zz&a+b=new", "GET", None)
        );
    }

    #[test]
    fn parse_extract는_잘못된_입력마다_사유를_돌려준다() {
        let one = json!([{ "name": "t" }]);
        let seven: Vec<Value> = (0..7).map(|i| json!({ "name": format!("f{i}") })).collect();
        let with_field = |field: Value| json!({ "selector": "a", "fields": [field] });
        let cases = [
            (json!([]), "extract must be an object"),
            (json!("li"), "extract must be an object"),
            (json!({}), "extract.selector is required"),
            (
                json!({ "selector": 5, "fields": one }),
                "extract.selector is required",
            ),
            (
                json!({ "selector": "  ", "fields": one }),
                "extract.selector is required",
            ),
            (
                json!({ "selector": "a".repeat(401), "fields": one }),
                "extract.selector is invalid",
            ),
            (
                json!({ "selector": "a", "limit": 5.5, "fields": one }),
                "extract.limit must be an integer from 1 to 20",
            ),
            (
                json!({ "selector": "a", "limit": "5", "fields": one }),
                "extract.limit must be an integer from 1 to 20",
            ),
            (
                json!({ "selector": "a" }),
                "extract.fields needs 1 to 6 fields",
            ),
            (
                json!({ "selector": "a", "fields": [] }),
                "extract.fields needs 1 to 6 fields",
            ),
            (
                json!({ "selector": "a", "fields": seven }),
                "extract.fields needs 1 to 6 fields",
            ),
            (with_field(json!("x")), "extract field must be an object"),
            (
                with_field(json!({ "name": "1a" })),
                "extract field name must be a short identifier",
            ),
            (
                with_field(json!({ "name": "a".repeat(42) })),
                "extract field name must be a short identifier",
            ),
            (
                json!({ "selector": "a", "fields": [{ "name": "t" }, { "name": "t" }] }),
                "duplicate extract field: t",
            ),
            (
                with_field(json!({ "name": "t", "selector": "  " })),
                "extract field t has a bad selector",
            ),
            (
                with_field(json!({ "name": "t", "selector": 5 })),
                "extract field t has a bad selector",
            ),
            (
                with_field(json!({ "name": "t", "attr": "1x" })),
                "extract field t has a bad attr",
            ),
            (
                with_field(json!({ "name": "t", "attr": 5 })),
                "extract field t has a bad attr",
            ),
        ];
        for (raw, error) in cases {
            assert_eq!(parse_extract(&raw), Err(error.to_owned()), "{raw}");
        }
        assert_eq!(parse_extract(&Value::Null), Ok(None));
    }

    #[test]
    fn 셀렉터는_다듬어_저장하지만_필드_셀렉터의_길이와_제어_문자는_다듬기_전에_본다() {
        let parsed = parse_extract(&json!({
            "selector": "\n a b \t",
            "limit": 20.0,
            "fields": [{ "name": "t", "selector": " .h ", "attr": "xlink:href" }]
        }));
        let expected = CliExtract {
            selector: "a b".to_owned(),
            limit: 20,
            fields: vec![field("t", Some(".h"), Some("xlink:href"))],
        };
        assert_eq!(parsed, Ok(Some(expected)));
        let nulls = json!({ "selector": "a", "limit": null, "fields": [{ "name": "t", "selector": null, "attr": null }] });
        let plain = CliExtract {
            selector: "a".to_owned(),
            limit: 5,
            fields: vec![field("t", None, None)],
        };
        assert_eq!(parse_extract(&nulls), Ok(Some(plain)));
        let with_selector = |selector: String| {
            parse_extract(
                &json!({ "selector": "a", "fields": [{ "name": "t", "selector": selector }] }),
            )
        };
        let bad = Err("extract field t has a bad selector".to_owned());
        assert_eq!(with_selector("x\n".to_owned()), bad);
        assert_eq!(with_selector(format!(" {}  ", "x".repeat(199))), bad);
        assert!(with_selector(format!(" {} ", "x".repeat(198))).is_ok());
    }

    #[test]
    fn 캡처_값은_경로와_본문에서도_읽고_안전하지_않은_값은_버린다() {
        let string = || optional(CliParamKind::String);
        let command = CliCommand {
            params: record([("id0", param(CliParamKind::String, true))]),
            query: record([("q", string()), ("big", string())]),
            body: Some(CliBody {
                json: Some(record(
                    ["title", "n", "f", "e", "s", "o", "c"].map(|name| (name, string())),
                )),
                form: None,
            }),
            ..command("post-items", "POST", "/items/{id0}")
        };
        let captured = captured_flag_values(
            &command,
            &[
                CapturedFlagSource {
                    url: format!(
                        "https://shop.example/items/%E0%A4%A?q=%20hi%20&big={}",
                        "x".repeat(501)
                    ),
                    request_post_data: Some(
                        r#"{"title":"T","n":5.0,"f":0.1,"e":1e21,"s":1e-7,"o":{"a":1},"c":"a\nb"}"#
                            .to_owned(),
                    ),
                    request_headers: None,
                },
                CapturedFlagSource {
                    url: "https://shop.example/items/a%20b?q=later&big=ok".to_owned(),
                    ..CapturedFlagSource::default()
                },
                CapturedFlagSource {
                    url: "bad".to_owned(),
                    ..CapturedFlagSource::default()
                },
            ],
        );
        // The broken escape stays raw, the 501-character value waits for a later
        // call, the object and the value with a line break are left out, and
        // numbers read as String(value).
        let expected = strings([
            ("id0", "%E0%A4%A"),
            ("q", "hi"),
            ("title", "T"),
            ("n", "5"),
            ("f", "0.1"),
            ("e", "1e+21"),
            ("s", "1e-7"),
            ("big", "ok"),
        ]);
        assert_eq!(keys(&captured), keys(&expected));
        assert_eq!(captured, expected);
    }

    #[test]
    fn 숫자는_자바스크립트_string처럼_적는다() {
        let cases = [
            (5.0, "5"),
            (-0.0, "0"),
            (1.5, "1.5"),
            (-1.5e-10, "-1.5e-10"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (123456789012345680000.0, "123456789012345680000"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (0.30000000000000004, "0.30000000000000004"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            // Two 17-digit strings are equally near; JavaScript takes the even one.
            (1972165110973718.0 + 0.25, "1972165110973718.2"),
            (1972165110973718.0 + 0.75, "1972165110973718.8"),
            (276394009951685.0 + 0.625, "276394009951685.62"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for (value, text) in cases {
            assert_eq!(js_number(value), text);
        }
    }

    #[test]
    fn uri_디코딩은_깨진_이스케이프를_거부하고_인코딩은_예약_문자를_바꾼다() {
        assert_eq!(decode_uri_component("a%20b%C3%A9"), Some("a bé".to_owned()));
        assert_eq!(decode_uri_component("100%"), None);
        assert_eq!(decode_uri_component("%zz"), None);
        assert_eq!(decode_uri_component("%E0%A4%A"), None);
        assert_eq!(decode_uri_component("%ff"), None);
        // A lead byte can't be completed by a literal character.
        assert_eq!(decode_uri_component("%C3é"), None);
        assert_eq!(
            encode_uri_component("a b/é!~*'()$&"),
            "a%20b%2F%C3%A9!~*'()%24%26"
        );
    }

    #[test]
    fn html_판정은_앞_300자와_자바스크립트_공백을_따른다() {
        let tail = "z".repeat(2_000);
        let cut = |body: String| present_raw_body(&body, false).body_cut;
        assert!(cut(format!("{}<HTML>{tail}", " ".repeat(290))));
        // Only "<ht" falls inside the first 300 characters.
        assert!(!cut(format!("{}<html>{tail}", " ".repeat(297))));
        // U+FEFF is whitespace in JavaScript and U+0085 is not.
        assert!(cut(format!("\u{feff}<Body>{tail}")));
        assert!(!cut(format!("\u{85}<body>{tail}")));
        assert!(!cut(format!("<html>{}", "z".repeat(1_494))));
        assert!(cut(format!("<html>{}", "z".repeat(1_495))));
        assert!(present_raw_body("<html>", true).body_cut);
        assert!(!present_raw_body("", true).body_cut);
    }

    #[test]
    fn 자르는_자리에_걸친_서로게이트_쌍은_통째로_뺀다() {
        // JavaScript would keep the lone high surrogate; a Rust string can't.
        let body = format!("<head>{}😀tail", "x".repeat(1_493));
        let shown = present_raw_body(&body, false);
        assert!(shown.body_cut);
        assert_eq!(shown.body, format!("<head>{}", "x".repeat(1_493)));
        let whole = format!("<head>{}😀tail", "x".repeat(1_492));
        assert_eq!(
            utf16_len(&present_raw_body(&whole, false).body),
            RAW_PREVIEW_CHARS
        );
    }

    #[test]
    fn json_뷰어_판정은_자바스크립트_공백을_다듬고_빈_items도_추출로_본다() {
        let shown = |body: &str| run_json_body(&run_body(body)).is_some();
        assert!(shown("\u{feff}{}"));
        assert!(shown("\u{a0}[]\u{3000}"));
        assert!(!shown("\u{85}{}"));
        // JSON.parse turns 1e999 into Infinity and takes a lone surrogate escape.
        assert!(shown("[1e999]"));
        assert!(shown(r#"{"a":"\ud800"}"#));
        assert!(shown(&format!(
            "{}{}",
            "[".repeat(3_000),
            "]".repeat(3_000)
        )));
        for broken in [
            "{}x",
            "[1,]",
            "{'a':1}",
            "[01]",
            "[1.]",
            "[\u{a0}1]",
            "{",
            "\"str\"",
            "",
        ] {
            assert!(!shown(broken), "{broken}");
        }
        let empty_items = CliRunBody {
            items: Some(Vec::new()),
            ..run_body("{}")
        };
        assert_eq!(run_json_body(&empty_items), None);
        let not_cut = CliRunBody {
            body_cut: Some(false),
            ..run_body("{}")
        };
        assert_eq!(run_json_body(&not_cut), Some("{}"));
    }

    #[test]
    fn 추출_값은_500자에서_자르고_이름_붙은_필드와_숫자_키를_앞에_둔다() {
        let long = "y".repeat(600);
        let items = scrub_extract_items(
            &[strings([
                ("b", &long),
                ("2", "two"),
                ("1", "one"),
                ("jwt", "x eyJhbGciOiJIUzI1NiJ9.abc.def"),
                ("password", "p"),
            ])],
            &[field("missing", None, None), field("jwt", None, None)],
        );
        assert_eq!(keys(&items[0]), ["1", "2", "jwt", "b", "password"]);
        assert_eq!(items[0]["jwt"], "x ****");
        assert_eq!(items[0]["b"], "y".repeat(500));
        assert_eq!(items[0]["password"], "****");
        assert_eq!(scrub_extract_items(&[], &[]), Vec::<Record<String>>::new());
    }

    #[test]
    fn 추출_스크립트의_job은_json이고_get과_head에는_본문을_싣지_않는다() {
        let spec = ExtractSpec {
            url: "https://s.example/q".to_owned(),
            method: "POST".to_owned(),
            headers: strings([("x-csrf-token", "a\"b")]),
            body: Some("x=1".to_owned()),
            extract: CliExtract {
                selector: "li".to_owned(),
                limit: 3,
                fields: vec![field("t", Some("h2"), None), field("l", None, Some("href"))],
            },
        };
        let expr = build_extract_expression(&spec);
        let job = r#"{"url":"https://s.example/q","method":"POST","headers":{"x-csrf-token":"a\"b"},"body":"x=1","timeoutMs":15000,"maxChars":2000000,"selector":"li","limit":3,"fields":[{"name":"t","selector":"h2"},{"name":"l","attr":"href"}],"noteNone":"selector matched nothing","noteCapped":"page read was capped"}"#;
        assert!(expr.starts_with(&format!(
            "\n(async () => {{\n  const job = {job}\n  const ac = new AbortController()\n"
        )));
        assert!(expr.ends_with("\n  }\n})()\n"));
        // The two lines whose backslashes the TypeScript template literal escapes.
        assert!(expr.contains(r#"const ctm = /charset\s*=\s*["']?([^;"'\s]+)/i.exec("#));
        assert!(expr.contains(r"value = value.replace(/\s+/g, ' ').trim()"));
        assert_eq!(expr.lines().count(), 76);
        for method in ["GET", "HEAD"] {
            let expr = build_extract_expression(&ExtractSpec {
                method: method.to_owned(),
                ..spec.clone()
            });
            assert!(!expr.contains(r#""body":"#), "{method}");
        }
    }
}
