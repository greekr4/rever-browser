//! Port of `src/main/mcp/token-decode.ts` — pure token detection/decoding.
//!
//! The TypeScript reads the clock for the expiry note; here "now" is passed in.

use serde_json::{Value, json};

use crate::{decode_uri_component, is_truthy, js_trim, json_parse};

/// Node's `Buffer.from(s, 'base64')`: both alphabets, characters outside them
/// skipped, decoding stopped at the first `=`. It never fails.
fn decode_base64_lenient(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut bits, mut pending) = (0u32, 0u32);
    for c in s.bytes() {
        let sextet = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        bits = (bits << 6) | u32::from(sextet);
        pending += 6;
        if pending >= 8 {
            pending -= 8;
            out.push((bits >> pending) as u8);
        }
    }
    out
}

/// Decodes base64 or base64url, padded or not, to text; bytes that are not
/// UTF-8 become U+FFFD as in Node's `toString('utf8')`.
pub fn decode_base64_url(s: &str) -> String {
    String::from_utf8_lossy(&decode_base64_lenient(s)).into_owned()
}

pub fn try_parse_json(s: &str) -> Option<Value> {
    json_parse(s)
}

/// `new Date(ms).toISOString()`; None where JavaScript throws RangeError.
fn to_iso_string(ms: f64) -> Option<String> {
    if !ms.is_finite() || ms.abs() > 8.64e15 {
        return None;
    }
    let ms = ms.trunc() as i64;
    let (days, rem) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Days since the epoch to a proleptic Gregorian date (Hinnant's civil_from_days).
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let year = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{}{:06}", if year < 0 { '-' } else { '+' }, year.abs())
    };
    Some(format!(
        "{year}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    ))
}

/// The ` (exp: …)` note for a JWT payload, empty when it has no numeric `exp`.
/// None where the TypeScript throws: an `exp` outside the range of a Date.
pub fn format_expiry(payload: &Value, now_ms: u64) -> Option<String> {
    let Some(exp) = payload.get("exp").and_then(Value::as_f64) else {
        return Some(String::new());
    };
    let ms = exp * 1000.0;
    let iso = to_iso_string(ms)?;
    let expired = if ms.trunc() < now_ms as f64 {
        " — EXPIRED"
    } else {
        ""
    };
    Some(format!(" (exp: {iso}{expired})"))
}

fn decode_hex(s: &str) -> String {
    let nibble = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
    let bytes: Vec<u8> = s
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| pair.iter().fold(0, |byte, b| (byte << 4) | nibble(*b)))
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Detect and decode a token value. Returns a plain object describing the result.
pub fn decode_token(value: &str, now_ms: u64) -> Value {
    let v = js_trim(value);

    // JWT: 3 base64url segments separated by dots.
    let jwt_parts: Vec<&str> = v.split('.').collect();
    if let [header, payload, _signature] = jwt_parts.as_slice() {
        let header = try_parse_json(&decode_base64_url(header)).filter(is_truthy);
        let payload = try_parse_json(&decode_base64_url(payload)).filter(is_truthy);
        // An undatable `exp` falls through, as the throw does in the TypeScript.
        if let (Some(header), Some(payload)) = (header, payload)
            && let Some(expiry) = format_expiry(&payload, now_ms)
        {
            return json!({
                "type": "jwt",
                "header": header,
                "payload": payload,
                "note": format!("signature not verified{expiry}"),
                "raw": v,
            });
        }
    }

    // 2-segment token (`payload.signature`): no header segment, so the usual
    // JWT pattern does not match — but the payload is plain base64url JSON and
    // the signature is usually a raw HMAC. The admin-tool pattern.
    if let [payload, signature] = jwt_parts.as_slice()
        && let Some(payload) = try_parse_json(&decode_base64_url(payload)).filter(is_truthy)
    {
        // A raw HMAC-SHA256/512 is 32/64 bytes: 43/44 and 86/88 base64url/base64
        // characters. Anything else is probably not a signature at all, so the
        // value falls through to the other decoders instead of being mislabeled.
        let sig_hint = match signature.len() {
            43 | 44 => "43-char signature — consistent with base64url(SHA-256 HMAC)",
            86 | 88 => "86-char signature — consistent with base64url(SHA-512 HMAC)",
            _ => "not a signature length",
        };
        if sig_hint != "not a signature length" {
            let expiry = payload
                .get("expiresAt")
                .and_then(Value::as_u64)
                .map(|at_ms| {
                    if at_ms < now_ms {
                        " — EXPIRED".to_owned()
                    } else {
                        String::new()
                    }
                })
                .unwrap_or_default();
            return json!({
                "type": "signed-token",
                "format": "payload.signature (2 segments, no header)",
                "payload": payload,
                "note": format!("signature not verified ({sig_hint}){expiry}"),
                "raw": v,
            });
        }
    }

    // URL-encoded JSON.
    if v.contains('%')
        && let Some(decoded) = decode_uri_component(v)
    {
        return match try_parse_json(&decoded).filter(is_truthy) {
            Some(parsed) => json!({ "type": "url-encoded-json", "decoded": parsed, "raw": v }),
            None => json!({ "type": "url-encoded", "decoded": decoded, "raw": v }),
        };
    }

    // base64 (eyJ prefix is common for JSON).
    let base64_like = |b: u8| b.is_ascii_alphanumeric() || b"+/=_-".contains(&b);
    if v.len() >= 4 && v.bytes().all(base64_like) {
        let decoded = decode_base64_url(v);
        if let Some(parsed) = try_parse_json(&decoded).filter(is_truthy) {
            return json!({ "type": "base64-json", "decoded": parsed, "raw": v });
        }
        let printable = |c: char| matches!(c, ' '..='~' | '\t' | '\n' | '\r');
        if !decoded.is_empty() && decoded.chars().all(printable) {
            return json!({ "type": "base64", "decoded": decoded, "raw": v });
        }
    }

    // hex.
    if v.len() >= 8 && v.len().is_multiple_of(2) && v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return json!({ "type": "hex", "decoded": decode_hex(v), "raw": v });
    }

    json!({ "type": "unknown", "note": "Could not detect encoding", "raw": v })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
    // {"sub":"u1","exp":1700000000}
    const EXP_PAYLOAD: &str = "eyJzdWIiOiJ1MSIsImV4cCI6MTcwMDAwMDAwMH0";
    const EXP_MS: u64 = 1_700_000_000_000;

    // ── cases of src/main/mcp/pure-helpers.test.ts ──────────────────────────

    #[test]
    fn jwt를_디코딩하고_만료를_표시한다() {
        // {"alg":"HS256"} . {"sub":"1","exp":1} . sig  → exp far in the past
        let header = decode_base64_url("eyJhbGciOiJIUzI1NiJ9");
        assert_eq!(try_parse_json(&header).unwrap()["alg"], "HS256");
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIiwiZXhwIjoxfQ.sig";
        let out = decode_token(jwt, EXP_MS);
        assert_eq!(out["type"], "jwt");
        assert!(out["note"].as_str().unwrap().contains("EXPIRED"));
    }

    #[test]
    fn base64_json과_url_인코딩된_json을_디코딩한다() {
        assert_eq!(decode_token("eyJhIjoxfQ==", EXP_MS)["type"], "base64-json");
        assert_eq!(
            decode_token("%7B%22a%22%3A1%7D", EXP_MS)["type"],
            "url-encoded-json"
        );
    }

    #[test]
    fn 알_수_없는_값은_unknown으로_돌려준다() {
        assert_eq!(decode_token("???", EXP_MS)["type"], "unknown");
    }

    #[test]
    fn 숫자_exp가_없는_페이로드는_만료_표기가_없다() {
        assert_eq!(
            format_expiry(&json!({"sub": "1"}), EXP_MS).as_deref(),
            Some("")
        );
    }

    // ── not in the TypeScript suite ─────────────────────────────────────────

    #[test]
    fn base64와_base64url을_패딩_유무와_상관없이_디코딩한다() {
        assert_eq!(decode_base64_url("aGVsbG8gd29ybGQ"), "hello world");
        assert_eq!(decode_base64_url("aGVsbG8gd29ybGQ="), "hello world");
        assert_eq!(decode_base64_url("aMOpbGxvfn4_"), "héllo~~?");
        assert_eq!(decode_base64_url("aMOpbGxvfn4/"), "héllo~~?");
        assert_eq!(decode_base64_url(""), "");
    }

    #[test]
    fn base64_디코딩은_node처럼_관대하다() {
        // Reference values from Node's Buffer.from(…, 'base64').
        assert_eq!(decode_base64_url("ab=cd"), "i");
        assert_eq!(decode_base64_url("aGVs bG8"), "hello");
        assert_eq!(decode_base64_url("YQ==YQ=="), "a");
        assert_eq!(decode_base64_url("aGk$$$aGk"), "hi\u{1a}\u{1a}");
        assert_eq!(decode_base64_url("a"), "");
        // Bytes that are not UTF-8 become U+FFFD.
        assert_eq!(decode_base64_url("-_-_"), "\u{fffd}\u{fffd}\u{fffd}");
    }

    #[test]
    fn json이_아니면_파싱_결과가_없다() {
        assert_eq!(try_parse_json(r#"{"a":1}"#), Some(json!({"a": 1})));
        assert_eq!(try_parse_json("null"), Some(Value::Null));
        assert_eq!(try_parse_json("{oops"), None);
        assert_eq!(try_parse_json(""), None);
    }

    #[test]
    fn 날짜를_자바스크립트_iso_형식으로_쓴다() {
        // Reference values from new Date(ms).toISOString().
        assert_eq!(
            to_iso_string(1.7e12).as_deref(),
            Some("2023-11-14T22:13:20.000Z")
        );
        assert_eq!(
            to_iso_string(-1000.0).as_deref(),
            Some("1969-12-31T23:59:59.000Z")
        );
        assert_eq!(
            to_iso_string(951_782_400_000.0).as_deref(),
            Some("2000-02-29T00:00:00.000Z")
        );
        assert_eq!(
            to_iso_string(1.5).as_deref(),
            Some("1970-01-01T00:00:00.001Z")
        );
        assert_eq!(
            to_iso_string(-62_198_755_200_001.0).as_deref(),
            Some("-000002-12-31T23:59:59.999Z")
        );
        assert_eq!(
            to_iso_string(253_402_300_800_000.0).as_deref(),
            Some("+010000-01-01T00:00:00.000Z")
        );
        assert_eq!(
            to_iso_string(8.64e15).as_deref(),
            Some("+275760-09-13T00:00:00.000Z")
        );
        assert_eq!(to_iso_string(8.64e15 + 1.0), None);
        assert_eq!(to_iso_string(f64::NAN), None);
    }

    #[test]
    fn 만료_시각을_적고_지났으면_expired를_붙인다() {
        let payload = json!({"exp": 1_700_000_000});
        assert_eq!(
            format_expiry(&payload, EXP_MS - 1).as_deref(),
            Some(" (exp: 2023-11-14T22:13:20.000Z)")
        );
        // Expired only once the clock is strictly past it.
        assert_eq!(
            format_expiry(&payload, EXP_MS).as_deref(),
            Some(" (exp: 2023-11-14T22:13:20.000Z)")
        );
        assert_eq!(
            format_expiry(&payload, EXP_MS + 1).as_deref(),
            Some(" (exp: 2023-11-14T22:13:20.000Z — EXPIRED)")
        );
    }

    #[test]
    fn exp가_숫자가_아니면_만료_표기가_없다() {
        for payload in [
            json!({"exp": "soon"}),
            json!({}),
            json!([1]),
            json!("x"),
            Value::Null,
        ] {
            assert_eq!(format_expiry(&payload, 0).as_deref(), Some(""));
        }
        assert_eq!(format_expiry(&json!({"exp": 1e20}), 0), None);
    }

    #[test]
    fn jwt의_헤더와_페이로드를_디코딩한다() {
        let token = format!("  {HEADER}.{EXP_PAYLOAD}.c2ln\n");
        let out = decode_token(&token, EXP_MS + 1);
        assert_eq!(
            out.to_string(),
            concat!(
                r#"{"type":"jwt","header":{"alg":"HS256","typ":"JWT"},"#,
                r#""payload":{"sub":"u1","exp":1700000000},"#,
                r#""note":"signature not verified (exp: 2023-11-14T22:13:20.000Z — EXPIRED)","#,
                r#""raw":"eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ1MSIsImV4cCI6MTcwMDAwMDAwMH0.c2ln"}"#
            )
        );
        let live = decode_token(&token, 0);
        assert_eq!(
            live["note"],
            "signature not verified (exp: 2023-11-14T22:13:20.000Z)"
        );
        // {"sub":"u1"}: no exp, no expiry note.
        let no_exp = decode_token(&format!("{HEADER}.eyJzdWIiOiJ1MSJ9.sig"), 0);
        assert_eq!(no_exp["note"], "signature not verified");
    }

    #[test]
    fn two_segment_payload_signature_decodes_as_signed_token() {
        // The wargame admin token shape: base64url(JSON) + "." + 43-char HMAC-SHA256.
        let token = "eyJyb2xlIjoiYWRtaW4iLCJleHBpcmVzQXQiOjE3OTU3OTU5MDc4Nzd9.iOpgycmTOwEX0FNRiBfArC5prvLYAgBwwIvxCe7pFW4";
        let out = decode_token(token, 1_785_000_000_000);
        assert_eq!(out["type"], "signed-token");
        assert_eq!(out["payload"]["role"], "admin");
        assert_eq!(out["payload"]["expiresAt"], 1_795_795_907_877_u64);
        let note = out["note"].as_str().unwrap();
        assert!(note.contains("SHA-256"), "{note}");
        assert!(!note.contains("EXPIRED"), "{note}");
        // Not-a-signature second segments are left to the other decoders.
        assert_eq!(decode_token(&format!("{HEADER}.{EXP_PAYLOAD}"), 0)["type"], "unknown");
    }

    #[test]
    fn three_dot_parts_without_json_are_not_jwt() {

        assert_eq!(decode_token("a.b.c", 0)["type"], "unknown");
        // Header decodes to the falsy JSON value 0 ("MA").
        assert_eq!(
            decode_token(&format!("MA.{EXP_PAYLOAD}.sig"), 0)["type"],
            "unknown"
        );
        assert_eq!(
            decode_token(&format!("{HEADER}.{EXP_PAYLOAD}"), 0)["type"],
            "unknown"
        );
    }

    #[test]
    fn 날짜로_만들_수_없는_exp의_jwt는_다음_판별로_넘어간다() {
        // {"exp":1e20}: toISOString throws in the TypeScript and the catch falls through.
        let out = decode_token(&format!("{HEADER}.eyJleHAiOjFlMjB9.sig"), 0);
        assert_eq!(out["type"], "unknown");
        assert_eq!(out["note"], "Could not detect encoding");
    }

    #[test]
    fn url_인코딩된_json과_문자열을_구분한다() {
        let out = decode_token("%7B%22a%22%3A1%7D", 0);
        assert_eq!(
            out.to_string(),
            r#"{"type":"url-encoded-json","decoded":{"a":1},"raw":"%7B%22a%22%3A1%7D"}"#
        );
        let text = decode_token("hello%20world", 0);
        assert_eq!(
            (&text["type"], &text["decoded"]),
            (&json!("url-encoded"), &json!("hello world"))
        );
        // A falsy JSON value stays a string.
        assert_eq!(decode_token("%30", 0)["decoded"], json!("0"));
    }

    #[test]
    fn 잘못된_퍼센트_인코딩은_다음_판별로_넘어간다() {
        assert_eq!(decode_token("100%", 0)["type"], "unknown");
        assert_eq!(decode_token("%E0%A4%A", 0)["type"], "unknown");
    }

    #[test]
    fn base64_json과_base64_문자열을_구분한다() {
        let out = decode_token("eyJhIjoxfQ==", 0);
        assert_eq!(
            out.to_string(),
            r#"{"type":"base64-json","decoded":{"a":1},"raw":"eyJhIjoxfQ=="}"#
        );
        let text = decode_token("aGVsbG8gd29ybGQ", 0);
        assert_eq!(
            (&text["type"], &text["decoded"]),
            (&json!("base64"), &json!("hello world"))
        );
        // "[1,2]" is JSON too.
        assert_eq!(decode_token("WzEsMl0", 0)["decoded"], json!([1, 2]));
    }

    #[test]
    fn 출력할_수_없는_base64는_hex나_unknown으로_넘어간다() {
        // Decodes to bytes that are not printable ASCII, and is not hex.
        assert_eq!(decode_token("-_-_", 0)["type"], "unknown");
        // Shorter than four characters never counts as base64.
        assert_eq!(decode_token("abc", 0)["type"], "unknown");
    }

    #[test]
    fn 짝수_길이의_hex를_디코딩한다() {
        // Reference values from the TypeScript under Node; bytes that are not
        // UTF-8 become U+FFFD.
        let cases = [
            ("68656c6c6f", "hello"),
            ("deadbeef", "\u{7ad}\u{fffd}\u{fffd}"),
            ("DEADBEEF00", "\u{7ad}\u{fffd}\u{fffd}\0"),
            (
                "0123456789abcdef0123",
                "\u{1}#Eg\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{1}#",
            ),
        ];
        for (token, decoded) in cases {
            let out = decode_token(token, 0);
            assert_eq!(
                (&out["type"], &out["decoded"]),
                (&json!("hex"), &json!(decoded)),
                "{token}"
            );
        }
        // Odd length is not hex.
        assert_eq!(decode_token("abcdef012", 0)["type"], "unknown");
    }

    #[test]
    fn 참_거짓_판정이_갈리는_json_값을_타입스크립트처럼_분류한다() {
        // Reference values from the TypeScript's decodeToken.
        let cases = [
            // {} and [] are truthy: a JWT of empty objects is still a JWT.
            ("e30.e30.x", "jwt"),
            ("W10.W10.x", "jwt"),
            ("MQ.MQ.x", "jwt"),
            // Header "null" is falsy, and the dots rule out base64 and hex.
            ("bnVsbA.e30.", "unknown"),
            ("..", "unknown"),
            ("dHJ1ZQ", "base64-json"),
            ("ImEi", "base64-json"),
            // 0, "" and false parse but are falsy: plain base64 text.
            ("MA==", "base64"),
            ("IiI=", "base64"),
            ("ZmFsc2U", "base64"),
            ("aGVsbG8K", "base64"),
            // Decodes to NUL bytes: not printable.
            ("AAAA", "unknown"),
            ("0000000000", "hex"),
            ("abcd", "unknown"),
            ("%", "unknown"),
            ("%22x%22", "url-encoded-json"),
            ("%5B%5D", "url-encoded-json"),
            ("null%20", "url-encoded"),
            ("a%20b.c%2e.d", "url-encoded"),
            ("", "unknown"),
        ];
        for (token, kind) in cases {
            assert_eq!(decode_token(token, EXP_MS)["type"], kind, "{token}");
        }
        assert_eq!(decode_token("MA==", 0)["decoded"], json!("0"));
        assert_eq!(decode_token("IiI=", 0)["decoded"], json!("\"\""));
        assert_eq!(
            decode_token("a%20b.c%2e.d", 0)["decoded"],
            json!("a b.c..d")
        );
        // A fractional exp keeps its milliseconds.
        assert_eq!(
            decode_token("eyJhbGciOiJub25lIn0.eyJleHAiOjEuNX0.x", EXP_MS)["note"],
            "signature not verified (exp: 1970-01-01T00:00:01.500Z — EXPIRED)"
        );
    }

    #[test]
    fn 앞뒤_공백은_자바스크립트_기준으로_걷어낸다() {
        assert_eq!(decode_token("\u{feff} abc \u{a0}", 0)["raw"], "abc");
    }
}
