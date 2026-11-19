//! Port of `src/main/mcp/tools/decode.ts` — the `decode_token` tool.

use std::time::{SystemTime, UNIX_EPOCH};

use rever_analysis::token_decode::decode_token;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, ok};
use serde_json::{Value, json};

use crate::args::validate;
use crate::js::json_stringify;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

pub struct DecodeToken;

impl Tool for DecodeToken {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "decode_token",
            "Detect and decode a token value: JWT (3-segment base64url), base64, URL-encoded JSON, or hex. Returns type, decoded content, and expiry for JWTs.",
            json!({
                "type": "object",
                "properties": {
                    "value": {
                        "type": "string",
                        "description": "Token or encoded string to decode"
                    }
                },
                "required": ["value"],
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            let value = args.get("value").and_then(Value::as_str).unwrap_or("");
            // The TypeScript wrapped this in try/catch; the ported decoder
            // reports an undecodable value in its result instead of throwing.
            ok(json_stringify(&decode_token(value, now_ms()), true))
        })
    }
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(DecodeToken);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;

    async fn decode(value: &str) -> (bool, String) {
        run(
            &DecodeToken,
            &ToolContext::for_tests(),
            json!({ "value": value }),
        )
        .await
    }

    // Header {"alg":"HS256","typ":"JWT"}.
    const JWT_HEADER: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";

    #[tokio::test]
    async fn 만료된_jwt는_헤더와_페이로드와_만료_표시를_돌려준다() {
        // Payload {"sub": "1", "exp": 1700000000}; the text is what the TypeScript returned.
        let token = format!("{JWT_HEADER}.eyJzdWIiOiAiMSIsICJleHAiOiAxNzAwMDAwMDAwfQ.sig");
        let expected = format!(
            r#"{{
  "type": "jwt",
  "header": {{
    "alg": "HS256",
    "typ": "JWT"
  }},
  "payload": {{
    "sub": "1",
    "exp": 1700000000
  }},
  "note": "signature not verified (exp: 2023-11-14T22:13:20.000Z — EXPIRED)",
  "raw": "{token}"
}}"#
        );
        assert_eq!(decode(&token).await, (false, expected));
    }

    #[tokio::test]
    async fn 아직_유효한_jwt에는_만료_표시가_없고_숫자는_자바스크립트처럼_찍힌다() {
        // Payload {"sub": "1", "exp": 4102444800, "n": 1.5, "big": 12345678901234567890, "f": 1e+21}.
        let token = format!(
            "{JWT_HEADER}.eyJzdWIiOiAiMSIsICJleHAiOiA0MTAyNDQ0ODAwLCAibiI6IDEuNSwgImJpZyI6IDEyMzQ1Njc4OTAxMjM0NTY3ODkwLCAiZiI6IDFlKzIxfQ.sig"
        );
        let (failed, text) = decode(&token).await;
        assert!(!failed);
        assert!(text.contains(
            "\"payload\": {\n    \"sub\": \"1\",\n    \"exp\": 4102444800,\n    \"n\": 1.5,\n    \"big\": 12345678901234567000,\n    \"f\": 1e+21\n  },\n  \"note\": \"signature not verified (exp: 2100-01-01T00:00:00.000Z)\","
        ));
    }

    #[tokio::test]
    async fn base64와_url_인코딩과_hex를_알아본다() {
        assert_eq!(
            decode("aGVsbG8gd29ybGQ=").await.1,
            "{\n  \"type\": \"base64\",\n  \"decoded\": \"hello world\",\n  \"raw\": \"aGVsbG8gd29ybGQ=\"\n}"
        );
        assert_eq!(
            decode("eyJhIjoxfQ==").await.1,
            "{\n  \"type\": \"base64-json\",\n  \"decoded\": {\n    \"a\": 1\n  },\n  \"raw\": \"eyJhIjoxfQ==\"\n}"
        );
        assert_eq!(
            decode("%7B%22a%22%3A1%7D").await.1,
            "{\n  \"type\": \"url-encoded-json\",\n  \"decoded\": {\n    \"a\": 1\n  },\n  \"raw\": \"%7B%22a%22%3A1%7D\"\n}"
        );
        assert_eq!(
            decode("%E4%B8%96").await.1,
            "{\n  \"type\": \"url-encoded\",\n  \"decoded\": \"世\",\n  \"raw\": \"%E4%B8%96\"\n}"
        );
        assert_eq!(
            decode("68656c6c6f").await.1,
            "{\n  \"type\": \"hex\",\n  \"decoded\": \"hello\",\n  \"raw\": \"68656c6c6f\"\n}"
        );
    }

    #[tokio::test]
    async fn 알아볼_수_없는_값은_unknown으로_돌려준다() {
        let unknown = |raw: &str| {
            format!(
                "{{\n  \"type\": \"unknown\",\n  \"note\": \"Could not detect encoding\",\n  \"raw\": \"{raw}\"\n}}"
            )
        };
        assert_eq!(decode("plain text").await, (false, unknown("plain text")));
        assert_eq!(decode("a.b.c").await, (false, unknown("a.b.c")));
        // The value is trimmed first.
        assert_eq!(decode("   ").await, (false, unknown("")));
    }

    #[tokio::test]
    async fn value가_없거나_문자열이_아니면_검증_오류다() {
        let ctx = ToolContext::for_tests();
        let expected = |received: &str| {
            format!(
                "MCP error -32602: Input validation error: Invalid arguments for tool decode_token: [\n  {{\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"value\"\n    ],\n    \"message\": \"Invalid input: expected string, received {received}\"\n  }}\n]"
            )
        };
        assert_eq!(
            run(&DecodeToken, &ctx, json!({})).await,
            (true, expected("undefined"))
        );
        assert_eq!(
            run(&DecodeToken, &ctx, json!({ "value": 5 })).await,
            (true, expected("number"))
        );
    }
}
