//! Port of `src/main/mcp/secret-scan.ts`: credential and PII patterns, masking,
//! and the per-request scan behind the secret tools and session-log redaction.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::Serialize;
use serde_json::Value;

use crate::traffic::{StoredRequest, utf16_prefix_len};

pub struct Pattern {
    pub kind: &'static str,
    pub re: Regex,
}

fn compile(table: &[(&'static str, &str)]) -> Vec<Pattern> {
    table
        .iter()
        .map(|(kind, source)| Pattern {
            kind,
            // JavaScript's `\b` only knows ASCII word characters. The default
            // Unicode one sees no boundary between Hangul and a token glued to
            // it, and would let that token through. Its `\s` is `is_js_space`.
            re: Regex::new(
                &source
                    .replace(r"\b", r"(?-u:\b)")
                    .replace(r"\s", r"[\s\x{feff}--\x{85}]"),
            )
            .expect("the pattern tables are constants"),
        })
        .collect()
}

/// Credential / secret patterns. Each is anchored enough to keep false positives
/// low; generic assignments are gated on a key name so we don't flag every long
/// string. Ordered most-specific first.
pub static SECRET_PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    compile(&[
        (
            "private-key",
            r"-----BEGIN (?:RSA |EC |OPENSSH |DSA |PGP )?PRIVATE KEY-----",
        ),
        (
            "jwt",
            r"eyJ[a-zA-Z0-9_-]{10,}\.eyJ[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,}",
        ),
        ("aws-access-key-id", r"\bAKIA[0-9A-Z]{16}\b"),
        ("google-api-key", r"\bAIza[0-9A-Za-z_-]{35}\b"),
        ("github-token", r"\bgh[pousr]_[0-9A-Za-z]{36,}\b"),
        ("slack-token", r"\bxox[baprs]-[0-9A-Za-z-]{10,}\b"),
        ("stripe-key", r"\b[sprk]k_(?:live|test)_[0-9A-Za-z]{16,}\b"),
        (
            "supabase-publishable",
            r"\bsb_publishable_[0-9A-Za-z_-]{16,}\b",
        ),
        ("bearer-token", r"\bBearer\s+[A-Za-z0-9._~+/-]{16,}=*"),
        // Case-insensitivity is ASCII-only and scoped to the key name, the only
        // part of the pattern with letters — what JavaScript's `i` flag does here.
        (
            "generic-secret-assignment",
            r#"["']?(?i-u:api[_-]?key|apikey|secret|access[_-]?token|auth[_-]?token|client[_-]?secret|password|passwd)["']?\s*[:=]\s*["'][^"'\s]{8,}["']"#,
        ),
    ])
});

/// JSON keys that name sensitive data. Reported even when the value is null or
/// empty — a field like `pinCode: null` in a create-response proves the server
/// stores it, which is the finding a red-team report wants.
pub fn is_sensitive_field(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "pin", "pincode", "otp", "otpcode", "password", "passwd", "secret",
        "apikey", "api_key", "token", "refreshtoken", "accesstoken",
    ]
    .iter()
    .any(|name| key.contains(name))
}

fn walk_fields(value: &Value, visit: &mut impl FnMut(&str, &Value)) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                visit(key, value);
                walk_fields(value, visit);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk_fields(item, visit);
            }
        }
        _ => {}
    }
}

/// The sensitive JSON fields of a body, with values masked the same way the
/// value scanners mask.
fn scan_sensitive_fields(body: &str, location: &str) -> Vec<Hit> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    walk_fields(&value, &mut |key, value| {
        if !is_sensitive_field(key) {
            return;
        }
        let masked = match value {
            Value::Null => "<null>".to_owned(),
            Value::String(text) if text.is_empty() => "<empty>".to_owned(),
            Value::String(text) => mask(text),
            other => mask(&other.to_string()),
        };
        hits.push(Hit {
            kind: format!("field:{key}"),
            location: location.to_owned(),
            masked,
        });
    });
    hits
}

/// Request headers that routinely carry credentials.
pub fn is_sensitive_header(name: &str) -> bool {
    [
        "authorization",
        "cookie",
        "x-api-key",
        "x-auth-token",
        "x-csrf-token",
        "proxy-authorization",
    ]
    .iter()
    .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
}

/// PII beyond credentials. Session logs persist to disk, so we also strip the
/// user data (emails, phones, OAuth refresh/access tokens) that reversing
/// transcripts routinely carry. Best-effort — not a guarantee no secret leaks.
pub static PII_PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    compile(&[
        (
            "email",
            r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b",
        ),
        // `[0-9]`, not `\d`: JavaScript's `\d` is ASCII-only.
        ("phone-kr", r"\b01[0-9][-\s]?[0-9]{3,4}[-\s]?[0-9]{4}\b"),
        (
            "oauth-token-field",
            r#"["']?(?i-u:[a-z]*_?(?:refresh|access)[_-]?token)["']?\s*[:=]\s*["']?[A-Za-z0-9._~+/-]{12,}["']?"#,
        ),
    ])
});

/// What JavaScript's `\s` and `trim()` call whitespace: Unicode White_Space
/// without U+0085 (NEL), plus U+FEFF (BOM).
fn is_js_space(c: char) -> bool {
    c == '\u{feff}' || (c.is_whitespace() && c != '\u{85}')
}

pub fn mask(s: &str) -> String {
    let trimmed = s.trim_matches(is_js_space);
    // Counted the way JavaScript's `length` counts, so the reported size matches.
    let length = trimmed.encode_utf16().count();
    if length <= 10 {
        return "****".into();
    }
    // `slice(0, 4)` and `slice(-4)` count UTF-16 units too: an emoji is two.
    let head = trimmed
        .get(..utf16_prefix_len(trimmed, 4))
        .unwrap_or_default();
    let mut units = 0;
    let tail = trimmed
        .char_indices()
        .rev()
        .take_while(|(_, c)| {
            units += c.len_utf16();
            units <= 4
        })
        .last()
        .and_then(|(start, _)| trimmed.get(start..))
        .unwrap_or_default();
    format!("{head}…{tail} ({length} chars)")
}

/// Replace every credential/PII match in free text with a masked marker that
/// carries no quotes/newlines, so redacting a JSON string value keeps it valid.
pub fn redact(text: &str) -> String {
    let mut out = text.to_owned();
    for pattern in SECRET_PATTERNS.iter().chain(PII_PATTERNS.iter()) {
        let replaced = pattern.re.replace_all(&out, |found: &Captures| {
            format!("‹{}:{}›", pattern.kind, mask(&found[0]))
        });
        if let std::borrow::Cow::Owned(replaced) = replaced {
            out = replaced;
        }
    }
    out
}

/// Redact every string in a value in its natural (unescaped) form before it is
/// re-serialized — catches secrets embedded in JSON that escaped quotes would hide.
pub fn redact_deep(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact(text)),
        Value::Array(items) => items.iter().map(redact_deep).collect(),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| (key.clone(), redact_deep(value)))
            .collect(),
        other => other.clone(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Hit {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "where")]
    pub location: String,
    pub masked: String,
}

pub fn scan_text(text: &str, location: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for pattern in SECRET_PATTERNS.iter() {
        for found in pattern.re.find_iter(text) {
            hits.push(Hit {
                kind: pattern.kind.to_owned(),
                location: location.to_owned(),
                masked: mask(found.as_str()),
            });
            // Per-source cap.
            if hits.len() > 50 {
                return hits;
            }
        }
    }
    hits
}

pub fn scan_request(entry: &StoredRequest) -> Vec<Hit> {
    let mut hits = Vec::new();
    // Response body (text only — skip binary/base64).
    if let Some(body) = &entry.response_body
        && entry.response_body_base64 != Some(true)
    {
        hits.extend(scan_text(body, "response-body"));
        hits.extend(scan_sensitive_fields(body, "response-body"));
    }
    if let Some(post_data) = &entry.request_post_data {
        hits.extend(scan_text(post_data, "request-body"));
        hits.extend(scan_sensitive_fields(post_data, "request-body"));
    }
    // Sensitive request headers, reported by name regardless of value shape.
    for (name, value) in entry.request_headers.iter().flatten() {
        if is_sensitive_header(name) && !value.is_empty() {
            hits.push(Hit {
                kind: format!("header:{}", name.to_lowercase()),
                location: "request-header".into(),
                masked: mask(value),
            });
        }
    }
    // Dedupe identical hits within one request.
    let mut seen = HashSet::new();
    hits.retain(|hit| seen.insert(hit.clone()));
    hits
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn bearer_토큰을_마스킹한다() {
        let out = redact("curl -H \"Authorization: Bearer 6c70a033abcd1234efgh5678\" x");
        assert!(!out.contains("6c70a033abcd1234efgh5678"));
        assert!(out.contains("‹bearer-token:"));
    }

    #[test]
    fn jwt를_마스킹한다() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghij";
        assert!(!redact(&format!("token={jwt}")).contains(jwt));
    }

    #[test]
    fn 이메일과_휴대폰_번호를_마스킹한다() {
        let out = redact("연락처: hong@example.com / 010-1234-5678");
        assert!(!out.contains("hong@example.com"));
        assert!(!out.contains("010-1234-5678"));
        assert!(out.contains("‹email:"));
        assert!(out.contains("‹phone-kr:"));
    }

    #[test]
    fn oauth_refresh_token_필드_값을_마스킹한다() {
        let out = redact("\"naverRefreshToken\":\"AAAABBBBCCCCDDDDEEEE1234\"");
        assert!(!out.contains("AAAABBBBCCCCDDDDEEEE1234"));
    }

    #[test]
    fn 비밀값이_없으면_원문을_보존한다() {
        let plain = "GET /v1/jobs 는 공개 목록입니다";
        assert_eq!(redact(plain), plain);
    }

    #[test]
    fn 중첩된_문자열만_마스킹하고_구조는_유지한다() {
        let input = json!({
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "text": "Bearer 6c70a033abcd1234efgh" }
            },
            "n": 42
        });
        let out = redact_deep(&input);
        assert_eq!(out["n"], 42);
        assert_eq!(out["update"]["sessionUpdate"], "agent_message_chunk");
        let text = out["update"]["content"]["text"].as_str().unwrap();
        assert!(!text.contains("6c70a033abcd1234efgh"));
    }

    // The cases below are not in the TypeScript suite. Their expected values
    // were taken from running the TypeScript on the same inputs.

    #[test]
    fn 공백은_자바스크립트의_정의를_따른다() {
        // U+FEFF is whitespace to JavaScript, U+0085 is not; Unicode says the reverse.
        assert_eq!(
            redact("Bearer\u{feff}6c70a033abcd1234efgh5678"),
            "‹bearer-token:Bear…5678 (31 chars)›"
        );
        let nel = "Bearer\u{85}6c70a033abcd1234efgh5678";
        assert_eq!(redact(nel), nel);
        let bom = "password=\"abc\u{feff}defghij\"";
        assert_eq!(redact(bom), bom);
        assert_eq!(
            redact("password=\"abc\u{85}defghij\""),
            "‹generic-secret-assignment:pass…hij\" (22 chars)›"
        );
        assert_eq!(
            mask("\u{feff}abcdefghijklm\u{85}"),
            "abcd…klm\u{85} (14 chars)"
        );
    }

    #[test]
    fn 마스크의_앞뒤_네_글자는_utf16_단위로_센다() {
        assert_eq!(mask("😀😀😀😀😀😀"), "😀😀…😀😀 (12 chars)");
        assert_eq!(mask("a😀bcdefghij😀k"), "a😀b…j😀k (15 chars)");
        assert_eq!(
            mask("가나다라마바사아자차카타"),
            "가나다라…자차카타 (12 chars)"
        );
        assert_eq!(mask("12345678901"), "1234…8901 (11 chars)");
        assert_eq!(mask("  short  "), "****");
    }

    #[test]
    fn 패턴은_자바스크립트와_같은_곳에서_걸린다() {
        for (text, expected) in [
            (
                "한글AKIAABCDEFGHIJKLMNOP한글",
                "한글‹aws-access-key-id:AKIA…MNOP (20 chars)›한글",
            ),
            (
                "PASSWORD = \"secretpass\"",
                "‹generic-secret-assignment:PASS…ass\" (23 chars)›",
            ),
            (
                "Passwd:\"비밀번호비밀번호\"",
                "‹generic-secret-assignment:Pass…밀번호\" (17 chars)›",
            ),
            // Arabic-Indic digits are not `\d` to JavaScript; 13 digits have no boundary.
            (
                "010-١٢٣٤-5678 01012345678 010 123 4567 0101234567890",
                "010-١٢٣٤-5678 ‹phone-kr:0101…5678 (11 chars)› ‹phone-kr:010 …4567 (12 chars)› 0101234567890",
            ),
            (
                "x_REFRESH-TOKEN = abcdefghijkl",
                "‹oauth-token-field:x_RE…ijkl (30 chars)›",
            ),
            // `i` without `u` folds ASCII only: long s and the Kelvin sign stay apart.
            (
                "ſecret=\"abcdefgh1\" api\u{212a}ey=\"abcdefgh1\"",
                "ſecret=\"abcdefgh1\" api\u{212a}ey=\"abcdefgh1\"",
            ),
            (
                "bearer 6c70a033abcd1234efgh5678 Bearer   6c70a033abcd1234efgh5678==",
                "bearer 6c70a033abcd1234efgh5678 ‹bearer-token:Bear…78== (35 chars)›",
            ),
            (
                "한hong@example.com한 a@b.co",
                "한‹email:hong….com (16 chars)›한 ‹email:****›",
            ),
        ] {
            assert_eq!(redact(text), expected);
        }
    }

    #[test]
    fn 한_출처에서_51건까지만_보고한다() {
        let keys: Vec<String> = (0..60).map(|i| format!("AKIA{i:016}")).collect();
        assert_eq!(scan_text(&keys.join(" "), "w").len(), 51);
    }

    // The TypeScript suite leaves the scan untested: one check
    // that the translated patterns, the header rule and the dedupe hold together.
    #[test]
    fn 요청을_훑어_본문과_헤더의_비밀값을_중복_없이_보고한다() {
        let entry = StoredRequest {
            response_body: Some("한글AKIAABCDEFGHIJKLMNOP 그리고 AKIAABCDEFGHIJKLMNOP".into()),
            request_post_data: Some("{\"password\":\"hunter2hunter2\"}".into()),
            request_headers: Some(
                [
                    ("Cookie".to_owned(), "sid=abcdefghijklmnop".to_owned()),
                    ("Accept".to_owned(), "*/*".to_owned()),
                ]
                .into(),
            ),
            ..StoredRequest::default()
        };
        let hits: Vec<(String, String, String)> = scan_request(&entry)
            .into_iter()
            .map(|hit| (hit.kind, hit.location, hit.masked))
            .collect();
        let hit = |kind: &str, location: &str, masked: &str| {
            (kind.to_owned(), location.to_owned(), masked.to_owned())
        };
        assert_eq!(
            hits,
            [
                hit("aws-access-key-id", "response-body", "AKIA…MNOP (20 chars)"),
                hit(
                    "generic-secret-assignment",
                    "request-body",
                    "\"pas…er2\" (27 chars)"
                ),
                hit(
                    "field:password", "request-body", "hunt…ter2 (14 chars)"
                ),
                hit("header:cookie", "request-header", "sid=…mnop (20 chars)"),
            ]
        );

        let base64 = StoredRequest {
            response_body_base64: Some(true),
            ..entry
        };
        assert_eq!(scan_request(&base64).len(), 3);
    }
}
