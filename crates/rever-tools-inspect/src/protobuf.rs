//! Port of `src/main/mcp/tools/protobuf.ts` — the `protobuf_decode` tool.

use rever_core::traffic::TrafficStore;
use rever_formats::protobuf_decode::{ProtobufDecodeOptions, decode_protobuf};
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Value, json};

use crate::args::validate;
use crate::js::{base64_bytes, hex_bytes, js_trim, json_stringify, latin1_bytes};
use crate::traffic;

/// Response body first, request body as fallback — gRPC calls carry both.
fn bytes_for_request(store: &TrafficStore, request_id: &str) -> Result<Vec<u8>, String> {
    let Some(entry) = store.get(request_id) else {
        return Err(format!("unknown requestId: {request_id}"));
    };
    if let Some(body) = entry.response_body.as_deref().filter(|b| !b.is_empty()) {
        return Ok(if entry.response_body_base64 == Some(true) {
            base64_bytes(body.encode_utf16())
        } else {
            latin1_bytes(body)
        });
    }
    if let Some(post_data) = entry.request_post_data.as_deref().filter(|p| !p.is_empty()) {
        return Ok(latin1_bytes(post_data));
    }
    Err("that request has no captured body (try get_request first, or pass data + encoding)".into())
}

fn protobuf_decode(ctx: &ToolContext, args: &Value) -> ToolResult {
    let text = |name: &str| args.get(name).and_then(Value::as_str);
    let buf = if let Some(data) = text("data") {
        let data = js_trim(data).encode_utf16();
        let buf = match text("encoding") {
            Some("hex") => hex_bytes(data),
            _ => base64_bytes(data),
        };
        if buf.is_empty() {
            return err("data decoded to zero bytes — wrong encoding?");
        }
        buf
    } else if let Some(request_id) = text("requestId").filter(|id| !id.is_empty()) {
        match bytes_for_request(&traffic(ctx), request_id) {
            Ok(bytes) => bytes,
            Err(message) => return err(message),
        }
    } else {
        return err("pass either requestId or data");
    };

    let options = ProtobufDecodeOptions {
        grpc_web: args.get("grpcWeb").and_then(Value::as_bool),
        ..Default::default()
    };
    let mut result = match serde_json::to_value(decode_protobuf(&buf, options)) {
        Ok(result) => result,
        Err(e) => return err(e.to_string()),
    };
    if let Some(result) = result.as_object_mut() {
        result.insert(
            "legend".into(),
            json!(
                "Keys are #<field-number>. kind: varint (value = raw, signed = zigzag) | fixed32/fixed64 (uint + float/double) | message (nested) | string | bytes. A varint may really be a bool or an enum."
            ),
        );
    }
    ok(json_stringify(&result, true))
}

pub struct ProtobufDecode;

impl Tool for ProtobufDecode {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "protobuf_decode",
            "Decode an opaque protobuf / gRPC-web body into a field-number → value structure without a .proto schema (the `protoc --decode_raw` equivalent). Pass a captured requestId (response body, falling back to the request body) or raw bytes as base64/hex. gRPC framing (1-byte flag + 4-byte big-endian length, including gRPC-web-text base64) is detected and stripped automatically. Keys are `#<field-number>`; length-delimited values are shown as a nested message, a string, or raw bytes, and varints carry both the raw and the zigzag-signed reading.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": {
                        "description": "Captured request whose body is protobuf / gRPC-web",
                        "type": "string"
                    },
                    "data": {
                        "description": "Raw body bytes (alternative to requestId)",
                        "type": "string"
                    },
                    "encoding": {
                        "description": "How `data` is encoded (default: base64)",
                        "type": "string",
                        "enum": ["base64", "hex"]
                    },
                    "grpcWeb": {
                        "description": "Force (true) or skip (false) gRPC frame parsing. Auto-detected when omitted.",
                        "type": "boolean"
                    }
                },
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            match validate(&self.def(), &args) {
                Ok(()) => protobuf_decode(ctx, &args),
                Err(invalid) => invalid,
            }
        })
    }
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ProtobufDecode);
}

#[cfg(test)]
mod tests {
    use rever_core::traffic::RequestPatch;

    use super::*;
    use crate::testing::{request, run, store};

    // Field 1 varint 150, field 2 "testing", field 3 nested {1: 42}, field 5
    // fixed32 1.5, field 6 fixed64 2.5, field 7 bytes 00 ff.
    const MESSAGE: &str = "CJYBEgd0ZXN0aW5nGgIIKi0AAMA/MQAAAAAAAARAOgIA/w==";
    // The same message as a gRPC data frame, followed by a trailer frame.
    const GRPC: &str =
        "AAAAACIIlgESB3Rlc3RpbmcaAggqLQAAwD8xAAAAAAAABEA6AgD/AQAAABBncnBjLXN0YXR1czogMA0K";
    const LEGEND: &str = "\"legend\": \"Keys are #<field-number>. kind: varint (value = raw, signed = zigzag) | fixed32/fixed64 (uint + float/double) | message (nested) | string | bytes. A varint may really be a bool or an enum.\"\n}";

    async fn decode(ctx: &ToolContext, args: Value) -> (bool, String) {
        run(&ProtobufDecode, ctx, args).await
    }

    fn body(
        id: &str,
        response: Option<&str>,
        base64: bool,
        post_data: Option<&str>,
    ) -> RequestPatch {
        RequestPatch {
            response_body: response.map(str::to_owned),
            response_body_base64: base64.then_some(true),
            request_post_data: post_data.map(str::to_owned),
            ..request(id, "https://api.test/grpc")
        }
    }

    #[tokio::test]
    async fn base64_데이터를_필드_구조로_풀고_범례를_붙인다() {
        // The text the TypeScript tool returned for the same call.
        let expected = r##"{
  "grpcWeb": false,
  "byteLength": 34,
  "frames": [
    {
      "byteLength": 34,
      "fields": {
        "#1": {
          "kind": "varint",
          "value": 150,
          "signed": 75
        },
        "#2": {
          "kind": "string",
          "value": "testing"
        },
        "#3": {
          "kind": "message",
          "fields": {
            "#1": {
              "kind": "varint",
              "value": 42,
              "signed": 21
            }
          }
        },
        "#5": {
          "kind": "fixed32",
          "hex": "0x0000c03f",
          "uint32": 1069547520,
          "float": 1.5
        },
        "#6": {
          "kind": "fixed64",
          "hex": "0x0000000000000440",
          "uint64": "4612811918334230528",
          "double": 2.5
        },
        "#7": {
          "kind": "bytes",
          "base64": "AP8=",
          "hex": "00ff",
          "length": 2
        }
      }
    }
  ],
  "legend": "Keys are #<field-number>. kind: varint (value = raw, signed = zigzag) | fixed32/fixed64 (uint + float/double) | message (nested) | string | bytes. A varint may really be a bool or an enum."
}"##;
        let ctx = ToolContext::for_tests();
        let (failed, text) = decode(&ctx, json!({ "data": MESSAGE })).await;
        assert!(!failed);
        assert_eq!(text, expected);
    }

    #[tokio::test]
    async fn hex_인코딩과_앞뒤_공백을_받아들인다() {
        let ctx = ToolContext::for_tests();
        let (_, from_base64) = decode(&ctx, json!({ "data": MESSAGE })).await;
        let hex = "0896011207 74657374696e67 1a02082a 2d0000c03f 310000000000000440 3a0200ff"
            .replace(' ', "");
        let padded = format!("\u{feff}{hex} \n");
        let (failed, from_hex) = decode(&ctx, json!({ "data": padded, "encoding": "hex" })).await;
        assert!(!failed);
        assert_eq!(from_hex, from_base64);
        let padded = format!("  {MESSAGE}\n");
        assert_eq!(decode(&ctx, json!({ "data": padded })).await.1, from_base64);
    }

    #[tokio::test]
    async fn 실수는_자바스크립트가_찍는_모양으로_나온다() {
        // Doubles 1, 1e21, 1e-7, NaN, 1e16; float 0.1.
        let data = "MQAAAAAAAPA/MVDv4tbkGktEMUivvJry13o+MQAAAAAAAPh/MQCA4Dd5w0FDLc3MzD0=";
        let ctx = ToolContext::for_tests();
        let (failed, text) = decode(&ctx, json!({ "data": data })).await;
        assert!(!failed);
        let doubles: Vec<&str> = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("\"double\": "))
            .collect();
        assert_eq!(doubles, ["1", "1e+21", "1e-7", "null", "10000000000000000"]);
        assert!(text.contains("\"float\": 0.10000000149011612\n"));
    }

    #[tokio::test]
    async fn grpc_프레임은_자동으로_벗기고_트레일러를_글자로_보여준다() {
        let ctx = ToolContext::for_tests();
        let (failed, text) = decode(&ctx, json!({ "data": GRPC })).await;
        assert!(!failed);
        assert!(text.starts_with("{\n  \"grpcWeb\": true,\n  \"byteLength\": 60,\n  \"frames\": [\n    {\n      \"byteLength\": 34,\n      \"flag\": 0,\n      \"fields\": {"));
        assert!(text.contains("    {\n      \"byteLength\": 16,\n      \"flag\": 1,\n      \"trailer\": true,\n      \"trailerText\": \"grpc-status: 0\\r\\n\"\n    }\n  ],\n"));
        assert!(text.ends_with(LEGEND));
    }

    #[tokio::test]
    async fn grpc_web_인자로_프레임_해석을_강제하거나_끈다() {
        let ctx = ToolContext::for_tests();
        let (failed, forced) = decode(&ctx, json!({ "data": MESSAGE, "grpcWeb": true })).await;
        assert!(!failed);
        assert_eq!(
            forced,
            format!(
                "{{\n  \"grpcWeb\": false,\n  \"byteLength\": 34,\n  \"frames\": [\n    {{\n      \"byteLength\": 34,\n      \"error\": \"not a valid sequence of gRPC frames (1-byte flag + 4-byte big-endian length)\"\n    }}\n  ],\n  {LEGEND}"
            )
        );
        let (_, skipped) = decode(&ctx, json!({ "data": GRPC, "grpcWeb": false })).await;
        assert_eq!(
            skipped,
            format!(
                "{{\n  \"grpcWeb\": false,\n  \"byteLength\": 60,\n  \"frames\": [\n    {{\n      \"byteLength\": 60,\n      \"error\": \"zero field number\"\n    }}\n  ],\n  {LEGEND}"
            )
        );
    }

    #[tokio::test]
    async fn 영_바이트로_풀리면_인코딩을_의심하라고_알린다() {
        let ctx = ToolContext::for_tests();
        for args in [
            json!({ "data": "" }),
            json!({ "data": "!!!" }),
            json!({ "data": "zz", "encoding": "hex" }),
        ] {
            assert_eq!(
                decode(&ctx, args).await,
                (
                    true,
                    "data decoded to zero bytes — wrong encoding?".to_owned()
                )
            );
        }
    }

    #[tokio::test]
    async fn 잘못된_인코딩으로_읽으면_읽힌_만큼만_해석한다() {
        // Base64 text read as hex: "AAAAAC" is three zero bytes, then "II" stops it.
        let ctx = ToolContext::for_tests();
        let (failed, text) = decode(&ctx, json!({ "data": GRPC, "encoding": "hex" })).await;
        assert!(!failed);
        assert!(text.contains("\"byteLength\": 3,\n      \"error\": \"truncated varint\""));
    }

    #[tokio::test]
    async fn data도_request_id도_없으면_거부한다() {
        let ctx = ToolContext::for_tests();
        for args in [json!({}), json!({ "requestId": "" })] {
            assert_eq!(
                decode(&ctx, args).await,
                (true, "pass either requestId or data".to_owned())
            );
        }
    }

    #[tokio::test]
    async fn data가_있으면_request_id보다_먼저_쓴다() {
        let ctx = ToolContext::for_tests();
        store(&ctx, body("pb1", Some(GRPC), true, None));
        let (failed, text) = decode(&ctx, json!({ "data": "", "requestId": "pb1" })).await;
        assert!(failed);
        assert_eq!(text, "data decoded to zero bytes — wrong encoding?");
    }

    #[tokio::test]
    async fn 요청의_응답_본문을_먼저_쓰고_없으면_요청_본문을_쓴다() {
        let ctx = ToolContext::for_tests();
        store(
            &ctx,
            body("pb1", Some(GRPC), true, Some("\u{8}\u{96}\u{1}")),
        );
        store(
            &ctx,
            body(
                "pb2",
                Some("\u{8}\u{96}\u{1}\u{12}\u{7}testing"),
                false,
                Some("ignored"),
            ),
        );
        store(&ctx, body("pb3", None, false, Some("\u{8}\u{96}\u{1}")));

        let (_, from_data) = decode(&ctx, json!({ "data": GRPC })).await;
        assert_eq!(
            decode(&ctx, json!({ "requestId": "pb1" })).await,
            (false, from_data)
        );

        // A text body is read as one byte per character, so U+0096 is 0x96.
        let (failed, text) = decode(&ctx, json!({ "requestId": "pb2" })).await;
        assert!(!failed);
        assert!(text.contains("\"byteLength\": 12,"));
        assert!(text.contains("\"value\": 150,"));
        assert!(text.contains("\"value\": \"testing\""));

        let (failed, text) = decode(&ctx, json!({ "requestId": "pb3" })).await;
        assert!(!failed);
        assert_eq!(
            text,
            format!(
                "{{\n  \"grpcWeb\": false,\n  \"byteLength\": 3,\n  \"frames\": [\n    {{\n      \"byteLength\": 3,\n      \"fields\": {{\n        \"#1\": {{\n          \"kind\": \"varint\",\n          \"value\": 150,\n          \"signed\": 75\n        }}\n      }}\n    }}\n  ],\n  {LEGEND}"
            )
        );
    }

    #[tokio::test]
    async fn 텍스트_본문은_코드_단위의_하위_바이트로_읽는다() {
        // U+0196 → 0x96, é → 0xE9, the emoji → 0x3D 0x00: six bytes in all.
        let ctx = ToolContext::for_tests();
        store(&ctx, body("pb6", Some("\u{8}\u{196}\u{1}é😀"), false, None));
        let (failed, text) = decode(&ctx, json!({ "requestId": "pb6" })).await;
        assert!(!failed);
        assert!(text.contains("\"byteLength\": 6,\n      \"error\": \"truncated 64-bit value\""));
    }

    #[tokio::test]
    async fn 본문이_없는_요청과_모르는_요청은_오류다() {
        let ctx = ToolContext::for_tests();
        store(&ctx, body("pb4", None, false, None));
        store(&ctx, body("pb5", Some(""), false, Some("")));
        let no_body =
            "that request has no captured body (try get_request first, or pass data + encoding)";
        for id in ["pb4", "pb5"] {
            assert_eq!(
                decode(&ctx, json!({ "requestId": id })).await,
                (true, no_body.to_owned())
            );
        }
        assert_eq!(
            decode(&ctx, json!({ "requestId": "nope" })).await,
            (true, "unknown requestId: nope".to_owned())
        );
    }

    #[tokio::test]
    async fn 타입이_틀린_인자는_검증_오류로_돌려준다() {
        let ctx = ToolContext::for_tests();
        let args = json!({ "data": 5, "encoding": "utf8", "grpcWeb": "yes", "requestId": 1 });
        let (failed, text) = decode(&ctx, args).await;
        assert!(failed);
        assert!(text.starts_with(
            "MCP error -32602: Input validation error: Invalid arguments for tool protobuf_decode: ["
        ));
        let messages: Vec<&str> = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("\"message\": "))
            .collect();
        assert_eq!(
            messages,
            [
                "\"Invalid input: expected string, received number\"",
                "\"Invalid input: expected string, received number\"",
                "\"Invalid option: expected one of \\\"base64\\\"|\\\"hex\\\"\"",
                "\"Invalid input: expected boolean, received string\"",
            ]
        );
    }
}
