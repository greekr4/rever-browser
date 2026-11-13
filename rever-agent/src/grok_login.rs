//! Port of `src/main/grok-login.ts` — the grok.com subscription stored by `grok login`.
//!
//! grok.com subscription stored by `grok login` at ~/.grok/auth.json.
//! The OIDC access token is a bearer for https://api.x.ai/v1. The token never
//! reaches the UI; callers in the app process read it here.
//!
//! The token refresh is an HTTP POST. This crate has no HTTP client, so the
//! caller passes the transport in: it receives a [`GrokRefreshRequest`] and
//! returns the parsed response, or None for a failed request or non-2xx status.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::agent_login::{LoginInfo, now_ms, read_file_or_none};

pub const GROK_PLAN: &str = "grok.com";
const REFRESH_SKEW_MS: i64 = 60_000;
/// The largest time value a JavaScript `Date` holds, in either direction.
const MAX_DATE_MS: i64 = 8_640_000_000_000_000;

fn grok_auth_path_in(home: &Path) -> PathBuf {
    home.join(".grok").join("auth.json")
}

/// None when the home directory cannot be determined.
pub fn grok_auth_path() -> Option<PathBuf> {
    std::env::home_dir().map(|home| grok_auth_path_in(&home))
}

/// The fields of an OAuth token response that are used. Deliberately neither
/// `Debug` nor `Serialize`: it carries tokens.
#[derive(Clone, Default, PartialEq)]
pub struct TokenResponse {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<f64>,
}

impl TokenResponse {
    /// Reads a response body leniently, as the untyped TypeScript does: a field
    /// of the wrong type is dropped rather than failing the whole response.
    pub fn from_value(body: &Value) -> Self {
        let string = |key: &str| {
            body.get(key)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        Self {
            access_token: string("access_token"),
            refresh_token: string("refresh_token"),
            expires_in: body.get("expires_in").and_then(Value::as_f64),
        }
    }
}

/// The POST that trades a refresh token for a new access token.
#[derive(Clone, PartialEq, Eq)]
pub struct GrokRefreshRequest {
    pub url: String,
    /// `application/x-www-form-urlencoded` body.
    pub body: String,
}

impl GrokRefreshRequest {
    pub const CONTENT_TYPE: &'static str = "application/x-www-form-urlencoded";
}

/// What [`apply_grok_token_response`] produced: the new file contents and the token.
#[derive(Clone, PartialEq, Eq)]
pub struct AppliedGrokToken {
    pub raw: String,
    pub access_token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrokCredentialSource {
    #[serde(rename = "subscription")]
    Subscription,
    #[serde(rename = "api-key")]
    ApiKey,
}

#[derive(Clone, PartialEq, Eq)]
pub struct GrokCredential {
    pub token: String,
    pub source: GrokCredentialSource,
}

fn parse_root(raw: &str) -> Option<Map<String, Value>> {
    let mut value = serde_json::from_str(raw).ok()?;
    js_key_order(&mut value);
    match value {
        Value::Object(root) => Some(root),
        _ => None,
    }
}

/// A key JavaScript treats as an array index: a canonical integer below 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    let index = key.parse::<u32>().ok()?;
    (index != u32::MAX && index.to_string() == key).then_some(index)
}

/// Reorders object keys the way `JSON.parse` lists them: array-index keys first
/// in ascending order, then the rest in document order. It decides which entry
/// is "first" and the order the file is written back in.
fn js_key_order(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(js_key_order),
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = std::mem::take(map).into_iter().collect();
            entries
                .iter_mut()
                .for_each(|(_, child)| js_key_order(child));
            // Stable sort: keys that are not indices keep their order.
            entries.sort_by_key(|(key, _)| array_index(key).map_or((1, 0), |index| (0, index)));
            map.extend(entries);
        }
        _ => {}
    }
}

/// JavaScript truthiness of a field that may be missing.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

/// A string field, or "" when it is missing or not a string.
fn text<'a>(entry: &'a Value, key: &str) -> &'a str {
    entry.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The entry to use: the first OIDC one holding a token, else the first holding any.
fn pick_entry(root: &Map<String, Value>) -> Option<(&str, &Value)> {
    let has_token = |entry: &Value| truthy(entry.get("key")) || truthy(entry.get("refresh_token"));
    let rows = || root.iter().filter(|(_, entry)| entry.is_object());
    rows()
        .find(|(_, entry)| text(entry, "auth_mode") == "oidc" && has_token(entry))
        .or_else(|| rows().find(|(_, entry)| has_token(entry)))
        .map(|(id, entry)| (id.as_str(), entry))
}

/// Shape check only. A live refresh token still counts after the access token expires.
pub fn read_grok_login(raw: &str, now_ms: i64) -> LoginInfo {
    let Some(root) = parse_root(raw) else {
        return LoginInfo::NO_LOGIN;
    };
    let Some((_, entry)) = pick_entry(&root) else {
        return LoginInfo::NO_LOGIN;
    };
    let key = text(entry, "key");
    let refresh = text(entry, "refresh_token");
    if key.is_empty() && refresh.is_empty() {
        return LoginInfo::NO_LOGIN;
    }
    let expired = !key.is_empty()
        && parse_iso_ms(text(entry, "expires_at")).is_some_and(|expires_at| expires_at <= now_ms);
    if expired && refresh.is_empty() {
        return LoginInfo::NO_LOGIN;
    }
    LoginInfo::logged_in(Some(GROK_PLAN))
}

fn read_grok_login_file_at(path: &Path, now_ms: i64) -> LoginInfo {
    read_file_or_none(path).map_or(LoginInfo::NO_LOGIN, |raw| read_grok_login(&raw, now_ms))
}

pub fn read_grok_login_file() -> LoginInfo {
    grok_auth_path().map_or(LoginInfo::NO_LOGIN, |path| {
        read_grok_login_file_at(&path, now_ms())
    })
}

fn usable_key(entry: &Value, now_ms: i64) -> Option<&str> {
    let key = text(entry, "key");
    if key.is_empty() {
        return None;
    }
    match parse_iso_ms(text(entry, "expires_at")) {
        Some(expires_at) if expires_at - REFRESH_SKEW_MS <= now_ms => None,
        _ => Some(key),
    }
}

/// Merge a token response into the auth file contents. Returns None when nothing usable came back.
pub fn apply_grok_token_response(
    raw: &str,
    token: &TokenResponse,
    now_ms: i64,
) -> Option<AppliedGrokToken> {
    let non_empty = |field: &Option<String>| field.clone().filter(|text| !text.is_empty());
    let access_token = non_empty(&token.access_token)?;
    let mut root = parse_root(raw)?;
    let id = pick_entry(&root)?.0.to_owned();
    let entry = root.get_mut(&id)?.as_object_mut()?;
    entry.insert("key".to_owned(), Value::String(access_token.clone()));
    if let Some(refresh_token) = non_empty(&token.refresh_token) {
        entry.insert("refresh_token".to_owned(), Value::String(refresh_token));
    }
    if let Some(expires_in) = token.expires_in.filter(|seconds| *seconds > 0.0) {
        let expires_at = now_ms as f64 + expires_in * 1000.0;
        // Outside the Date range `toISOString` throws in the TypeScript and the
        // refresh is abandoned; the range check also keeps the cast exact.
        if expires_at.abs() > MAX_DATE_MS as f64 {
            return None;
        }
        let expires_at = to_iso_string(expires_at.trunc() as i64)?;
        entry.insert("expires_at".to_owned(), Value::String(expires_at));
    }
    let root = Value::Object(root);
    // Keep the file in the style it was written in.
    let next = if raw.contains('\n') {
        format!("{}\n", serde_json::to_string_pretty(&root).ok()?)
    } else {
        serde_json::to_string(&root).ok()?
    };
    Some(AppliedGrokToken {
        raw: next,
        access_token,
    })
}

/// `URLSearchParams.toString()`: only `*-._` and ASCII alphanumerics stay
/// literal, a space becomes `+`.
fn form_urlencode(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (index, (name, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        for (part, text) in [name, value].into_iter().enumerate() {
            if part > 0 {
                out.push('=');
            }
            for byte in text.bytes() {
                match byte {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                        out.push(char::from(byte));
                    }
                    b' ' => out.push('+'),
                    _ => out.push_str(&format!("%{byte:02X}")),
                }
            }
        }
    }
    out
}

/// The refresh request for an auth entry, or None when it cannot be refreshed.
fn grok_refresh_request(entry: &Value) -> Option<GrokRefreshRequest> {
    let refresh = text(entry, "refresh_token");
    let client_id = text(entry, "oidc_client_id");
    if refresh.is_empty() || client_id.is_empty() {
        return None;
    }
    let issuer = Some(text(entry, "oidc_issuer"))
        .filter(|issuer| !issuer.is_empty())
        .unwrap_or("https://auth.x.ai");
    Some(GrokRefreshRequest {
        url: format!(
            "{}/oauth2/token",
            issuer.strip_suffix('/').unwrap_or(issuer)
        ),
        body: form_urlencode(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", client_id),
        ]),
    })
}

/// Replaces the auth file through a temp file in `tmp_dir`, keeping its permission bits.
fn write_auth(path: &Path, tmp_dir: &Path, next: &str) -> std::io::Result<()> {
    // Also what makes a vanished auth file an error instead of recreating it.
    let metadata = std::fs::metadata(path)?;
    let tmp = tmp_dir.join(format!(".grok-auth-{}.json", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(metadata.permissions().mode() & 0o777);
    }
    #[cfg(not(unix))]
    let _ = metadata;
    options.open(&tmp)?.write_all(next.as_bytes())?;
    std::fs::rename(&tmp, path)
}

fn ensure_grok_access_token_at(
    auth_path: &Path,
    tmp_dir: &Path,
    now_ms: i64,
    refresh: impl FnOnce(&GrokRefreshRequest) -> Option<TokenResponse>,
) -> Option<String> {
    let raw = read_file_or_none(auth_path)?;
    let root = parse_root(&raw)?;
    let (_, entry) = pick_entry(&root)?;
    if let Some(current) = usable_key(entry, now_ms) {
        return Some(current.to_owned());
    }

    let token = refresh(&grok_refresh_request(entry)?)?;
    let applied = apply_grok_token_response(&raw, &token, now_ms)?;
    // On a failed write, keep serving the new access token for this process.
    let _ = write_auth(auth_path, tmp_dir, &applied.raw);
    Some(applied.access_token)
}

/// Access token from `grok login`, refreshing it when it is inside the skew window.
pub fn ensure_grok_access_token(
    now_ms: i64,
    refresh: impl FnOnce(&GrokRefreshRequest) -> Option<TokenResponse>,
) -> Option<String> {
    ensure_grok_access_token_at(&grok_auth_path()?, &std::env::temp_dir(), now_ms, refresh)
}

fn pick_grok_credential(
    oauth: impl FnOnce() -> Option<String>,
    stored_key: Option<&str>,
) -> Option<GrokCredential> {
    let credential = |token: String, source| GrokCredential { token, source };
    oauth()
        .filter(|token| !token.is_empty())
        .map(|token| credential(token, GrokCredentialSource::Subscription))
        // Fall through to a pasted key or XAI_API_KEY.
        .or_else(|| {
            stored_key
                .filter(|key| !key.is_empty())
                .map(|key| credential(key.to_owned(), GrokCredentialSource::ApiKey))
        })
}

/// The subscription token when `grok login` has one, else the stored API key.
pub fn resolve_grok_credential(
    stored_key: Option<&str>,
    refresh: impl FnOnce(&GrokRefreshRequest) -> Option<TokenResponse>,
) -> Option<GrokCredential> {
    pick_grok_credential(|| ensure_grok_access_token(now_ms(), refresh), stored_key)
}

/// `Date.parse` for ISO 8601: a date, optionally a time, optionally `Z` or a
/// `±HH:mm` offset. Anything else is None, where JavaScript gives NaN — and
/// also for the legacy formats V8 would still accept ("Oct 19 2026").
/// A time without an offset is read as UTC (JavaScript uses the local zone).
fn parse_iso_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let number = |from: usize, to: usize| -> Option<i64> {
        let digits = bytes.get(from..to)?;
        digits
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| digits.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let is = |at: usize, expected: u8| bytes.get(at) == Some(&expected);

    let (mut year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    // Days past the month's end roll over into the next month, as V8 does.
    if !is(4, b'-') || !is(7, b'-') || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if bytes.len() == 10 {
        return Some(days_from_civil(year, month, day) * 86_400_000);
    }
    if !matches!(bytes.get(10), Some(b'T' | b't' | b' ')) || !is(13, b':') {
        return None;
    }
    // A space instead of `T` is not ISO: V8 falls back to its legacy parser,
    // which reads a year below 100 as a two-digit year.
    if is(10, b' ') && year < 100 {
        year += if year < 50 { 2000 } else { 1900 };
    }
    let (hour, minute) = (number(11, 13)?, number(14, 16)?);
    let (mut second, mut millis, mut at) = (0, 0, 16);
    if is(at, b':') {
        second = number(17, 19)?;
        at = 19;
        if is(at, b'.') {
            let digits = bytes.get(20..)?;
            let count = digits.iter().take_while(|d| d.is_ascii_digit()).count();
            // Milliseconds are the first three digits; the rest is dropped.
            millis = number(20, 20 + count.min(3))?
                * match count {
                    0 => return None,
                    1 => 100,
                    2 => 10,
                    _ => 1,
                };
            at = 20 + count;
        }
    }
    let end_of_day = hour == 24 && minute == 0 && second == 0 && millis == 0;
    if (hour > 23 && !end_of_day) || minute > 59 || second > 59 {
        return None;
    }
    let offset_minutes = match bytes.get(at..)? {
        [] | [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), rest @ ..] => {
            let (hours, minutes) = match rest.len() {
                5 if is(at + 3, b':') => (number(at + 1, at + 3)?, number(at + 4, at + 6)?),
                4 => (number(at + 1, at + 3)?, number(at + 3, at + 5)?),
                _ => return None,
            };
            if hours > 23 || minutes > 59 {
                return None;
            }
            (hours * 60 + minutes) * if *sign == b'-' { -1 } else { 1 }
        }
        _ => return None,
    };
    let date_ms = days_from_civil(year, month, day) * 86_400_000;
    Some(date_ms + ((hour * 60 + minute - offset_minutes) * 60 + second) * 1000 + millis)
}

/// `new Date(ms).toISOString()`; None outside the range a Date can hold.
fn to_iso_string(ms: i64) -> Option<String> {
    if !(-MAX_DATE_MS..=MAX_DATE_MS).contains(&ms) {
        return None;
    }
    let (year, month, day) = civil_from_days(ms.div_euclid(86_400_000));
    let in_day = ms.rem_euclid(86_400_000);
    // Years outside 0000–9999 take a sign and six digits.
    let year = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{}{:06}", if year < 0 { '-' } else { '+' }, year.abs())
    };
    Some(format!(
        "{year}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        in_day / 3_600_000,
        in_day / 60_000 % 60,
        in_day / 1000 % 60,
        in_day % 1000
    ))
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (Howard Hinnant's
/// days_from_civil).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`]: (year, month, day).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;
    use crate::test_support::TempDir;

    /// `Date.parse('2026-10-19T00:00:00.000Z')`
    const NOW: i64 = 1_792_368_000_000;
    const HOUR: i64 = 3_600_000;
    const ID: &str = "https://auth.x.ai::client";

    fn iso(ms: i64) -> String {
        to_iso_string(ms).unwrap_or_default()
    }

    fn blob(over: &[(&str, Value)]) -> String {
        let mut entry = json!({
            "key": "access",
            "refresh_token": "refresh",
            "expires_at": iso(NOW + HOUR),
            "auth_mode": "oidc",
            "oidc_client_id": "client",
            "oidc_issuer": "https://auth.x.ai"
        });
        if let Some(fields) = entry.as_object_mut() {
            for (key, value) in over {
                fields.insert((*key).to_owned(), value.clone());
            }
        }
        json!({ ID: entry }).to_string()
    }

    fn token(
        access: Option<&str>,
        refresh: Option<&str>,
        expires_in: Option<f64>,
    ) -> TokenResponse {
        TokenResponse {
            access_token: access.map(str::to_owned),
            refresh_token: refresh.map(str::to_owned),
            expires_in,
        }
    }

    fn grok_login() -> LoginInfo {
        LoginInfo::logged_in(Some("grok.com"))
    }

    /// The picked entry of an auth file, for reading back what was saved.
    fn saved_entry(raw: &str) -> Value {
        serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|root| root.get(ID).cloned())
            .unwrap_or_default()
    }

    // ── readGrokLogin ──

    #[test]
    fn 유효한_grok_com_로그인이면_구독으로_본다() {
        assert_eq!(parse_iso_ms("2026-10-19T00:00:00.000Z"), Some(NOW));
        assert_eq!(read_grok_login(&blob(&[]), NOW), grok_login());
    }

    #[test]
    fn 액세스_토큰이_만료돼도_리프레시_토큰이_있으면_로그인이다() {
        let raw = blob(&[("expires_at", json!(iso(NOW - HOUR)))]);
        assert_eq!(read_grok_login(&raw, NOW), grok_login());
    }

    #[test]
    fn 토큰이_둘_다_없으면_로그인이_아니다() {
        let raw = blob(&[("key", json!("")), ("refresh_token", json!(""))]);
        assert!(!read_grok_login(&raw, NOW).logged_in);
        assert_eq!(read_grok_login("not json", NOW), LoginInfo::NO_LOGIN);
        assert!(!read_grok_login("{}", NOW).logged_in);
    }

    // ── applyGrokTokenResponse ──

    #[test]
    fn 새_액세스_토큰과_만료_시각을_같은_항목에_넣는다() {
        let applied = apply_grok_token_response(
            &blob(&[]),
            &token(Some("next"), Some("rotated"), Some(120.0)),
            NOW,
        );
        assert!(applied.as_ref().is_some_and(|a| a.access_token == "next"));
        let entry = saved_entry(&applied.map(|a| a.raw).unwrap_or_default());
        assert_eq!(text(&entry, "key"), "next");
        assert_eq!(text(&entry, "refresh_token"), "rotated");
        assert_eq!(
            parse_iso_ms(text(&entry, "expires_at")).map(|at| at - NOW),
            Some(120_000)
        );
    }

    #[test]
    fn 액세스_토큰이_없으면_파일을_바꾸지_않는다() {
        assert!(apply_grok_token_response(&blob(&[]), &token(None, None, None), NOW).is_none());
    }

    // ── not covered by the TypeScript tests ──

    #[test]
    fn 만료된_액세스_토큰만_있으면_로그인이_아니다() {
        let expired = ("expires_at", json!(iso(NOW - HOUR)));
        let raw = blob(&[expired.clone(), ("refresh_token", json!(""))]);
        assert_eq!(read_grok_login(&raw, NOW), LoginInfo::NO_LOGIN);
        // Expiring exactly now counts as expired.
        let raw = blob(&[
            ("expires_at", json!(iso(NOW))),
            ("refresh_token", json!("")),
        ]);
        assert_eq!(read_grok_login(&raw, NOW), LoginInfo::NO_LOGIN);
        let raw = blob(&[
            ("expires_at", json!(iso(NOW + 1))),
            ("refresh_token", json!("")),
        ]);
        assert_eq!(read_grok_login(&raw, NOW), grok_login());
    }

    #[test]
    fn 만료_시각을_읽을_수_없으면_키가_있는_것만으로_로그인이다() {
        for expires_at in [json!("soon"), json!(""), json!(null), json!(12345)] {
            let raw = blob(&[("expires_at", expires_at), ("refresh_token", json!(""))]);
            assert_eq!(read_grok_login(&raw, NOW), grok_login(), "{raw}");
        }
    }

    #[test]
    fn 리프레시_토큰만_있어도_로그인이다() {
        let raw = blob(&[("key", json!(""))]);
        assert_eq!(read_grok_login(&raw, NOW), grok_login());
    }

    #[test]
    fn 문자열이_아닌_토큰은_토큰으로_치지_않는다() {
        // Truthy, so the entry is picked, but neither field is usable text.
        let raw = blob(&[("key", json!(42)), ("refresh_token", json!(true))]);
        assert_eq!(read_grok_login(&raw, NOW), LoginInfo::NO_LOGIN);
    }

    #[test]
    fn 객체가_아닌_루트는_로그인이_아니다() {
        for raw in ["[]", "null", "5", "\"x\"", "[{\"key\":\"a\"}]", ""] {
            assert_eq!(read_grok_login(raw, NOW), LoginInfo::NO_LOGIN, "{raw}");
        }
    }

    #[test]
    fn oidc_항목을_먼저_고르고_없으면_토큰이_있는_첫_항목을_고른다() {
        let root = parse_root(
            r#"{
                "z-legacy": {"key": "legacy"},
                "skipped": "text",
                "list": [{"key": "in-array"}],
                "empty-oidc": {"auth_mode": "oidc", "key": ""},
                "a-oidc": {"auth_mode": "oidc", "refresh_token": "r"},
                "b-oidc": {"auth_mode": "oidc", "key": "second"}
            }"#,
        )
        .unwrap_or_default();
        assert_eq!(pick_entry(&root).map(|(id, _)| id), Some("a-oidc"));

        let root = parse_root(r#"{"n": null, "empty": {}, "z": {"key": "k"}, "a": {"key": "k2"}}"#)
            .unwrap_or_default();
        assert_eq!(pick_entry(&root).map(|(id, _)| id), Some("z"));

        let root =
            parse_root(r#"{"a": {"key": ""}, "b": {"key": 0}, "c": []}"#).unwrap_or_default();
        assert!(pick_entry(&root).is_none());
    }

    #[test]
    fn 정수_모양_키는_자바스크립트처럼_오름차순으로_먼저_본다() {
        // Expected values are what the TypeScript returns for the same file.
        let raw = r#"{"b":{"auth_mode":"oidc","key":"kb"},"10":{"auth_mode":"oidc","key":"k10","z":1,"2":{"y":0,"1":0},"0":[{"b":0,"7":0}]},"9":{"key":"k9"},"01":{"auth_mode":"oidc","key":"k01"}}"#;
        let root = parse_root(raw).unwrap_or_default();
        assert_eq!(pick_entry(&root).map(|(id, _)| id), Some("10"));
        let applied = apply_grok_token_response(raw, &token(Some("next"), None, None), NOW);
        assert_eq!(
            applied.map(|a| a.raw).as_deref(),
            Some(
                r#"{"9":{"key":"k9"},"10":{"0":[{"7":0,"b":0}],"2":{"1":0,"y":0},"auth_mode":"oidc","key":"next","z":1},"b":{"auth_mode":"oidc","key":"kb"},"01":{"auth_mode":"oidc","key":"k01"}}"#
            )
        );

        let keys = r#"{"b":1,"4294967295":1,"4294967294":1,"10":1,"9":1,"01":1,"-1":1,"1.5":1,"+2":1,"0":1}"#;
        let order: Vec<String> = parse_root(keys)
            .unwrap_or_default()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            order,
            [
                "0",
                "9",
                "10",
                "4294967294",
                "b",
                "4294967295",
                "01",
                "-1",
                "1.5",
                "+2"
            ]
        );
    }

    #[test]
    fn 만료_한_분_전부터는_키를_쓰지_않는다() {
        let key_expiring_in = |ms: i64| {
            let raw = blob(&[("expires_at", json!(iso(NOW + ms)))]);
            usable_key(&saved_entry(&raw), NOW).map(str::to_owned)
        };
        assert_eq!(key_expiring_in(HOUR).as_deref(), Some("access"));
        assert_eq!(key_expiring_in(60_001).as_deref(), Some("access"));
        assert_eq!(key_expiring_in(60_000), None);
        assert_eq!(key_expiring_in(-HOUR), None);

        // No readable expiry: the key is used as it is.
        let raw = blob(&[("expires_at", json!("soon"))]);
        assert_eq!(usable_key(&saved_entry(&raw), NOW), Some("access"));
        let raw = blob(&[("key", json!(""))]);
        assert_eq!(usable_key(&saved_entry(&raw), NOW), None);
    }

    #[test]
    fn 한_줄짜리_파일은_한_줄로_키_순서를_지켜_다시_쓴다() {
        let raw = r#"{"z":{"key":"old","refresh_token":"r","note":"n/a"},"a":{"key":"other"}}"#;
        let applied = apply_grok_token_response(raw, &token(Some("new"), None, Some(1.5)), NOW);
        assert_eq!(
            applied.map(|a| a.raw).as_deref(),
            Some(
                r#"{"z":{"key":"new","refresh_token":"r","note":"n/a","expires_at":"2026-10-19T00:00:01.500Z"},"a":{"key":"other"}}"#
            )
        );
    }

    #[test]
    fn 여러_줄짜리_파일은_들여쓰기_두_칸과_끝_줄바꿈으로_다시_쓴다() {
        let raw = "{\n  \"z\": { \"refresh_token\": \"r\", \"scopes\": [], \"extra\": {} }\n}";
        let applied = apply_grok_token_response(raw, &token(Some("new"), Some("r2"), None), NOW);
        let expected = "{\n  \"z\": {\n    \"refresh_token\": \"r2\",\n    \"scopes\": [],\n    \"extra\": {},\n    \"key\": \"new\"\n  }\n}\n";
        assert_eq!(applied.map(|a| a.raw).as_deref(), Some(expected));
    }

    #[test]
    fn 응답에_없는_값은_기존_값을_그대로_둔다() {
        let before = saved_entry(&blob(&[]));
        for response in [
            token(Some("next"), None, None),
            token(Some("next"), Some(""), Some(0.0)),
            token(Some("next"), None, Some(-5.0)),
            token(Some("next"), None, Some(f64::NAN)),
        ] {
            let applied = apply_grok_token_response(&blob(&[]), &response, NOW);
            let entry = saved_entry(&applied.map(|a| a.raw).unwrap_or_default());
            assert_eq!(text(&entry, "key"), "next");
            assert_eq!(entry.get("refresh_token"), before.get("refresh_token"));
            assert_eq!(entry.get("expires_at"), before.get("expires_at"));
        }
    }

    #[test]
    fn 쓸_수_없는_응답이나_파일이면_none이다() {
        let next = token(Some("next"), None, None);
        assert!(apply_grok_token_response(&blob(&[]), &token(Some(""), None, None), NOW).is_none());
        assert!(apply_grok_token_response("not json", &next, NOW).is_none());
        assert!(apply_grok_token_response("[]", &next, NOW).is_none());
        assert!(apply_grok_token_response("{}", &next, NOW).is_none());
        assert!(apply_grok_token_response(r#"{"a":{"key":""}}"#, &next, NOW).is_none());
        // An expiry no Date can hold: the TypeScript throws out of toISOString.
        for seconds in [1e300, f64::INFINITY] {
            let far = token(Some("next"), None, Some(seconds));
            assert!(apply_grok_token_response(&blob(&[]), &far, NOW).is_none());
        }
    }

    #[test]
    fn 응답_본문은_타입이_맞는_필드만_읽는다() {
        let body = json!({ "access_token": "a", "refresh_token": "r", "expires_in": 3600 });
        assert!(TokenResponse::from_value(&body) == token(Some("a"), Some("r"), Some(3600.0)));
        let body = json!({ "access_token": 5, "refresh_token": "", "expires_in": "3600" });
        assert!(TokenResponse::from_value(&body) == TokenResponse::default());
        assert!(TokenResponse::from_value(&json!("error")) == TokenResponse::default());
    }

    #[test]
    fn 갱신_요청은_발급자의_토큰_주소로_폼을_보낸다() {
        let request = |over: &[(&str, Value)]| grok_refresh_request(&saved_entry(&blob(over)));
        let expected_body = "grant_type=refresh_token&refresh_token=refresh&client_id=client";
        let default = request(&[]);
        assert!(default.as_ref().is_some_and(|r| {
            r.url == "https://auth.x.ai/oauth2/token" && r.body == expected_body
        }));
        // Only one trailing slash is dropped; a missing issuer falls back.
        let url = |issuer: Value| request(&[("oidc_issuer", issuer)]).map(|r| r.url);
        assert_eq!(
            url(json!("https://id.example/")).as_deref(),
            Some("https://id.example/oauth2/token")
        );
        assert_eq!(
            url(json!("https://id.example//")).as_deref(),
            Some("https://id.example//oauth2/token")
        );
        assert_eq!(
            url(json!("")).as_deref(),
            Some("https://auth.x.ai/oauth2/token")
        );
        assert_eq!(
            url(json!(null)).as_deref(),
            Some("https://auth.x.ai/oauth2/token")
        );

        assert!(request(&[("refresh_token", json!(""))]).is_none());
        assert!(request(&[("oidc_client_id", json!(null))]).is_none());
    }

    #[test]
    fn 폼_인코딩은_urlsearchparams와_같다() {
        // Expected value is what Node prints for the same pairs.
        assert_eq!(
            form_urlencode(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", "a b+c/=~*-._é&"),
                ("client_id", "client"),
            ]),
            "grant_type=refresh_token&refresh_token=a+b%2Bc%2F%3D%7E*-._%C3%A9%26&client_id=client"
        );
    }

    // ── auth file on disk ──

    struct AuthFile {
        dir: TempDir,
        path: PathBuf,
    }

    impl AuthFile {
        fn new(tag: &str, contents: &str) -> Self {
            let dir = TempDir::new(tag);
            let path = dir.path().join("auth.json");
            assert!(std::fs::write(&path, contents).is_ok());
            Self { dir, path }
        }

        fn ensure(
            &self,
            refresh: impl FnOnce(&GrokRefreshRequest) -> Option<TokenResponse>,
        ) -> Option<String> {
            ensure_grok_access_token_at(&self.path, self.dir.path(), NOW, refresh)
        }

        fn contents(&self) -> String {
            std::fs::read_to_string(&self.path).unwrap_or_default()
        }
    }

    #[test]
    fn 유효한_키가_있으면_갱신하지_않고_그대로_돌려준다() {
        let file = AuthFile::new("grok-current", &blob(&[]));
        let called = Cell::new(false);
        let token = file.ensure(|_| {
            called.set(true);
            None
        });
        assert_eq!(token.as_deref(), Some("access"));
        assert!(!called.get());
        assert_eq!(file.contents(), blob(&[]));
    }

    #[test]
    fn 만료가_임박하면_갱신하고_파일을_새_토큰으로_바꾼다() {
        let stale = blob(&[("expires_at", json!(iso(NOW + 30_000)))]);
        let file = AuthFile::new("grok-refresh", &stale);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let private = std::fs::Permissions::from_mode(0o600);
            assert!(std::fs::set_permissions(&file.path, private).is_ok());
        }

        let seen_url = Cell::new(String::new());
        let refreshed = file.ensure(|request| {
            seen_url.set(request.url.clone());
            Some(token(Some("next"), Some("rotated"), Some(3600.0)))
        });
        assert_eq!(refreshed.as_deref(), Some("next"));
        assert_eq!(seen_url.take(), "https://auth.x.ai/oauth2/token");

        let entry = saved_entry(&file.contents());
        assert_eq!(text(&entry, "key"), "next");
        assert_eq!(text(&entry, "refresh_token"), "rotated");
        assert_eq!(text(&entry, "expires_at"), iso(NOW + HOUR));
        assert_eq!(text(&entry, "oidc_client_id"), "client");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file.path).map(|m| m.permissions().mode() & 0o777);
            assert_eq!(mode.ok(), Some(0o600));
        }

        // The saved token is now current: no second refresh.
        assert_eq!(file.ensure(|_| None).as_deref(), Some("next"));
        assert_eq!(read_grok_login_file_at(&file.path, NOW), grok_login());
    }

    #[test]
    fn 갱신에_실패하면_none이고_파일은_그대로다() {
        let stale = blob(&[("expires_at", json!(iso(NOW - HOUR)))]);
        let file = AuthFile::new("grok-refresh-fails", &stale);
        assert_eq!(file.ensure(|_| None), None);
        assert_eq!(file.ensure(|_| Some(token(None, Some("r"), None))), None);
        assert_eq!(file.contents(), stale);
    }

    #[test]
    fn 갱신할_수_없는_항목이면_요청을_보내지_않는다() {
        let stale = blob(&[
            ("expires_at", json!(iso(NOW - HOUR))),
            ("oidc_client_id", json!("")),
        ]);
        let file = AuthFile::new("grok-no-client", &stale);
        let called = Cell::new(false);
        let token = file.ensure(|_| {
            called.set(true);
            None
        });
        assert_eq!(token, None);
        assert!(!called.get());
    }

    #[test]
    fn 파일을_쓰지_못해도_새_토큰은_돌려준다() {
        let stale = blob(&[("expires_at", json!(iso(NOW - HOUR)))]);
        let file = AuthFile::new("grok-write-fails", &stale);
        let missing_tmp = file.dir.path().join("no-such-dir");
        let refreshed = ensure_grok_access_token_at(&file.path, &missing_tmp, NOW, |_| {
            Some(token(Some("next"), None, None))
        });
        assert_eq!(refreshed.as_deref(), Some("next"));
        assert_eq!(file.contents(), stale);
    }

    #[test]
    fn 인증_파일이_없거나_깨졌으면_none이다() {
        let dir = TempDir::new("grok-missing");
        let missing = dir.path().join("auth.json");
        let refresh = |_: &GrokRefreshRequest| Some(token(Some("next"), None, None));
        assert_eq!(
            ensure_grok_access_token_at(&missing, dir.path(), NOW, refresh),
            None
        );
        assert_eq!(read_grok_login_file_at(&missing, NOW), LoginInfo::NO_LOGIN);

        let file = AuthFile::new("grok-broken", "{{");
        assert_eq!(file.ensure(refresh), None);
        assert_eq!(
            read_grok_login_file_at(&file.path, NOW),
            LoginInfo::NO_LOGIN
        );
    }

    #[test]
    fn 인증_파일_경로는_홈_아래_grok_auth_json이다() {
        assert_eq!(
            grok_auth_path_in(Path::new("/home/u")),
            Path::new("/home/u").join(".grok").join("auth.json")
        );
    }

    // ── resolveGrokCredential ──

    #[test]
    fn 구독_토큰이_있으면_저장된_키보다_먼저_쓴다() {
        let picked = pick_grok_credential(|| Some("oauth".to_owned()), Some("xai-key"));
        assert!(picked.is_some_and(|c| {
            c.token == "oauth" && c.source == GrokCredentialSource::Subscription
        }));
    }

    #[test]
    fn 구독_토큰이_없으면_저장된_키를_쓰고_둘_다_없으면_none이다() {
        let picked = pick_grok_credential(|| None, Some("xai-key"));
        assert!(
            picked.is_some_and(|c| {
                c.token == "xai-key" && c.source == GrokCredentialSource::ApiKey
            })
        );
        assert!(pick_grok_credential(|| None, None).is_none());
        assert!(pick_grok_credential(|| None, Some("")).is_none());
        assert!(pick_grok_credential(|| Some(String::new()), None).is_none());
    }

    #[test]
    fn 자격_증명_출처는_타입스크립트와_같은_이름으로_직렬화한다() {
        let sources = [
            GrokCredentialSource::Subscription,
            GrokCredentialSource::ApiKey,
        ];
        assert_eq!(
            serde_json::to_value(sources).ok(),
            Some(json!(["subscription", "api-key"]))
        );
    }

    // ── Date.parse / toISOString ── expected values are what Node 24 prints.

    #[test]
    fn iso_날짜를_자바스크립트와_같은_밀리초로_읽는다() {
        for (text, expected) in [
            ("2026-10-19", 1_792_368_000_000),
            ("2026-10-19T00:00:00.000Z", 1_792_368_000_000),
            ("2026-10-19T01:00:00.123456789Z", 1_792_371_600_123),
            ("2026-10-19T01:00:00.5Z", 1_792_371_600_500),
            ("2026-10-19T01:00:00+09:00", 1_792_339_200_000),
            ("2026-10-19T01:00:00+0900", 1_792_339_200_000),
            ("2026-10-19T01:00:00-00:30", 1_792_373_400_000),
            ("2026-10-19t01:00:00z", 1_792_371_600_000),
            ("2026-10-19 01:00:00Z", 1_792_371_600_000),
            ("2026-10-19T01:00Z", 1_792_371_600_000),
            ("2026-02-31T00:00:00Z", 1_772_496_000_000),
            ("2026-10-19T24:00:00Z", 1_792_454_400_000),
            ("1969-12-31T23:59:59.999Z", -1),
            ("0000-01-01T00:00:00Z", -62_167_219_200_000),
            // With a space V8's legacy parser takes over: two-digit years.
            ("0049-10-19 01:00:00Z", 2_518_218_000_000),
            ("0050-10-19 01:00:00Z", -606_006_000_000),
            ("0099-10-19 01:00:00Z", 940_294_800_000),
            ("0100-10-19 01:00:00Z", -58_986_313_200_000),
            ("0000-10-19T01:00:00Z", -62_141_986_800_000),
            ("0000-10-19", -62_141_990_400_000),
        ] {
            assert_eq!(parse_iso_ms(text), Some(expected), "{text}");
        }
    }

    #[test]
    fn 읽을_수_없는_날짜는_none이다() {
        for text in [
            "",
            "soon",
            "2026-13-01",
            " 2026-10-19T01:00:00Z",
            "2026-10-19T01:00:00Z ",
            "2026-10-19T01:00:00.Z",
            "2026-10-19T01:00:00+09",
            "2026-10-19T01:00:60Z",
            "2026-10-19T24:00:01Z",
            "2026-10-19T01:00:00 Z",
            "20261019T010000Z",
            "2026-10-19T01:00:00+09:0０",
            "２０２６-10-19",
        ] {
            assert_eq!(parse_iso_ms(text), None, "{text:?}");
        }
    }

    #[test]
    fn 밀리초를_to_iso_string과_같은_문자열로_쓴다() {
        for (ms, expected) in [
            (0, "1970-01-01T00:00:00.000Z"),
            (1_792_368_120_000, "2026-10-19T00:02:00.000Z"),
            (1_792_368_000_123, "2026-10-19T00:00:00.123Z"),
            (-1, "1969-12-31T23:59:59.999Z"),
            (253_402_300_800_000, "+010000-01-01T00:00:00.000Z"),
            (-62_167_219_200_001, "-000001-12-31T23:59:59.999Z"),
            (MAX_DATE_MS, "+275760-09-13T00:00:00.000Z"),
        ] {
            assert_eq!(to_iso_string(ms).as_deref(), Some(expected), "{ms}");
        }
        assert_eq!(to_iso_string(MAX_DATE_MS + 1), None);
        assert_eq!(to_iso_string(i64::MIN), None);
    }
}
