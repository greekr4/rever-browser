//! Port of `src/main/mcp/tools/grpc_call.ts` — call a gRPC method directly over
//! HTTP/2 to reach a backend gRPC service the HTTP front talks to internally.
//!
//! This is a protocol client: the protobuf wire encoding and gRPC framing are the
//! same an ordinary gRPC library uses. It carries no payloads of its own.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{pretty, schema};

pub fn register(registry: &mut ToolRegistry) {
    registry.register(GrpcCall);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    String,
    Int,
    Bool,
    Bytes,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProtoField {
    pub number: u64,
    #[serde(rename = "type")]
    pub kind: FieldType,
    pub value: Value,
}

/// Standard LEB128 varint encoding.
pub fn encode_varint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value > 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            break;
        }
    }
    bytes
}

/// One protobuf field as its key and value bytes.
pub fn encode_field(field: &ProtoField) -> Result<Vec<u8>, String> {
    if field.number < 1 {
        return Err(format!("bad field number: {}", field.number));
    }
    match field.kind {
        FieldType::String | FieldType::Bytes => {
            let payload = match field.kind {
                FieldType::String => field.value.as_str().unwrap_or_default().as_bytes().to_vec(),
                // base64 → bytes, as the TypeScript decoded it.
                _ => base64::engine::general_purpose::STANDARD
                    .decode(field.value.as_str().unwrap_or_default())
                    .map_err(|_| format!("field {} is not valid base64", field.number))?,
            };
            let mut out = encode_varint((field.number << 3) | 2);
            out.extend(encode_varint(payload.len() as u64));
            out.extend(payload);
            Ok(out)
        }
        FieldType::Int | FieldType::Bool => {
            let number = match field.kind {
                FieldType::Bool => u64::from(field.value.as_bool().unwrap_or(false)),
                _ => {
                    let value = field.value.as_f64().unwrap_or(f64::NAN);
                    if value.fract() != 0.0 || value < 0.0 {
                        return Err(format!(
                            "int field {} must be a non-negative integer",
                            field.number
                        ));
                    }
                    value as u64
                }
            };
            let mut out = encode_varint(field.number << 3);
            out.extend(encode_varint(number));
            Ok(out)
        }
    }
}

/// Concatenate the encoded fields into a protobuf message.
pub fn encode_message(fields: &[ProtoField]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for field in fields {
        out.extend(encode_field(field)?);
    }
    Ok(out)
}

/// gRPC length-prefixed frame: 1 compression byte (0) + 4-byte BE length + message.
pub fn frame_message(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + message.len());
    out.push(0);
    out.extend((message.len() as u32).to_be_bytes());
    out.extend(message);
    out
}

/// Strip the 5-byte gRPC prefix from a response payload (the first frame).
pub fn unframe_message(buffer: &[u8]) -> Vec<u8> {
    if buffer.len() < 5 {
        return Vec::new();
    }
    let len = u32::from_be_bytes([buffer[1], buffer[2], buffer[3], buffer[4]]) as usize;
    buffer[5..(5 + len).min(buffer.len())].to_vec()
}

/// Pull length-delimited (wire type 2) field values out of a protobuf buffer as
/// UTF-8, a loose decode so the caller can read string reply fields.
pub fn extract_string_fields(buffer: &[u8]) -> BTreeMap<u64, String> {
    let mut out = BTreeMap::new();
    let mut i = 0;
    let read_varint = |i: &mut usize| -> u64 {
        let mut result = 0u64;
        let mut shift = 0;
        while *i < buffer.len() {
            let byte = buffer[*i];
            *i += 1;
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        result
    };
    while i < buffer.len() {
        let key = read_varint(&mut i);
        let field = key >> 3;
        match key & 0x7 {
            2 => {
                let len = read_varint(&mut i) as usize;
                let end = (i + len).min(buffer.len());
                out.insert(field, String::from_utf8_lossy(&buffer[i..end]).into_owned());
                i = end;
            }
            0 => {
                read_varint(&mut i);
            }
            5 => i += 4,
            1 => i += 8,
            _ => break,
        }
    }
    out
}

struct GrpcReply {
    http_status: u16,
    grpc_status: String,
    grpc_message: String,
    data: Vec<u8>,
}

/// Send the framed message to `target` over HTTP/2 and read the reply, including
/// the `grpc-status` trailer.
async fn send_grpc(
    target: &str,
    path: &str,
    body: Vec<u8>,
    tls: bool,
    timeout: Duration,
) -> Result<GrpcReply, String> {
    let call = async {
        let tcp = tokio::net::TcpStream::connect(target)
            .await
            .map_err(|e| e.to_string())?;
        let host = target.rsplit_once(':').map_or(target, |(host, _)| host);
        if tls {
            let stream = tls_connect(tcp, host).await?;
            drive(stream, host, target, path, body).await
        } else {
            drive(tcp, host, target, path, body).await
        }
    };
    tokio::time::timeout(timeout, call)
        .await
        .map_err(|_| format!("gRPC call timed out after {}ms", timeout.as_millis()))?
}

/// The HTTP/2 exchange, over whichever stream it is given.
async fn drive<S>(
    io: S,
    _host: &str,
    authority: &str,
    path: &str,
    body: Vec<u8>,
) -> Result<GrpcReply, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = h2::client::handshake(io).await.map_err(|e| e.to_string())?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let scheme = "http";
    let uri: http::Uri = format!("{scheme}://{authority}{path}")
        .parse()
        .map_err(|_| format!("invalid gRPC path: {path}"))?;
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(())
        .map_err(|e| e.to_string())?;
    let (response, mut body_stream) = sender
        .send_request(request, false)
        .map_err(|e| e.to_string())?;
    body_stream
        .send_data(body.into(), true)
        .map_err(|e| e.to_string())?;
    let response = response.await.map_err(|e| e.to_string())?;
    let http_status = response.status().as_u16();
    let header_status = grpc_header(response.headers());
    let mut body = response.into_body();
    let mut data = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        let _ = body.flow_control().release_capacity(chunk.len());
        data.extend_from_slice(&chunk);
    }
    let trailer_status = body
        .trailers()
        .await
        .ok()
        .flatten()
        .map(|trailers| grpc_header(&trailers));
    let (grpc_status, grpc_message) = trailer_status
        .filter(|(status, _)| !status.is_empty())
        .unwrap_or(header_status);
    Ok(GrpcReply {
        http_status,
        grpc_status,
        grpc_message,
        data,
    })
}

/// `(grpc-status, grpc-message)` from a header map; the message is percent-decoded.
fn grpc_header(headers: &http::HeaderMap) -> (String, String) {
    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    (value("grpc-status"), percent_decode(&value("grpc-message")))
}

/// `decodeURIComponent` for the ASCII escapes gRPC messages use.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn tls_connect(
    tcp: tokio::net::TcpStream,
    host: &str,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, String> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(crate::raw_http::AcceptAnyCert(provider)))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let name = rustls_pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| format!("invalid TLS server name: {host}"))?;
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(|e| e.to_string())
}

pub struct GrpcCall;

impl Tool for GrpcCall {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "grpc_call",
            "Call a gRPC method directly over HTTP/2 — reaches a backend gRPC service the HTTP front talks to internally (another port on the same host). Encodes scalar protobuf fields, frames the message, reads the reply and its grpc-status.",
            schema(
                json!({
                    "target": { "type": "string", "description": "host:port of the gRPC server, e.g. \"127.0.0.1:50045\"" },
                    "path": { "type": "string", "description": "gRPC path: /<package.>Service/Method, e.g. \"/RickyService/SubmitTestimonial\"" },
                    "fields": {
                        "type": "array",
                        "description": "scalar fields of the request message",
                        "items": {
                            "type": "object",
                            "properties": {
                                "number": { "type": "integer", "description": "protobuf field number (from the .proto)" },
                                "type": { "type": "string", "enum": ["string", "int", "bool", "bytes"] },
                                "value": {}
                            },
                            "required": ["number", "type", "value"]
                        }
                    },
                    "tls": { "type": "boolean", "description": "Use TLS (default false = plaintext h2c, right for local targets)" },
                    "timeoutMs": { "type": "integer", "description": "default 8000" }
                }),
                &["target", "path", "fields"],
            ),
        )
    }

    fn call<'a>(&'a self, _ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let Some(target) = args["target"].as_str().map(str::to_owned) else {
                return err("target is required");
            };
            let Some(path) = args["path"].as_str().map(str::to_owned) else {
                return err("path is required");
            };
            let fields: Vec<ProtoField> = match serde_json::from_value(args["fields"].clone()) {
                Ok(fields) => fields,
                Err(e) => return err(format!("bad fields: {e}")),
            };
            let message = match encode_message(&fields) {
                Ok(message) => message,
                Err(error) => return err(error),
            };
            let tls = args["tls"].as_bool().unwrap_or(false);
            let timeout = Duration::from_millis(args["timeoutMs"].as_u64().unwrap_or(8000));
            let reply = match send_grpc(&target, &path, frame_message(&message), tls, timeout).await
            {
                Ok(reply) => reply,
                Err(error) => return err(error),
            };
            let decoded = unframe_message(&reply.data);
            let reply_fields: serde_json::Map<String, Value> = extract_string_fields(&decoded)
                .into_iter()
                .map(|(number, value)| (number.to_string(), json!(value)))
                .collect();
            ok(pretty(&json!({
                "httpStatus": reply.http_status,
                "grpcStatus": if reply.grpc_status.is_empty() { "(none)".to_owned() } else { reply.grpc_status },
                "grpcMessage": reply.grpc_message,
                "replyFields": reply_fields,
                "replyBytes": decoded.len(),
            })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(number: u64, kind: FieldType, value: Value) -> ProtoField {
        ProtoField {
            number,
            kind,
            value,
        }
    }

    #[test]
    fn 표준_leb128_인코딩() {
        assert_eq!(encode_varint(0), [0x00]);
        assert_eq!(encode_varint(1), [0x01]);
        assert_eq!(encode_varint(127), [0x7f]);
        assert_eq!(encode_varint(128), [0x80, 0x01]);
        assert_eq!(encode_varint(300), [0xac, 0x02]);
    }

    #[test]
    fn string_필드는_wire_type_2() {
        assert_eq!(
            encode_field(&field(1, FieldType::String, json!("ab"))).unwrap(),
            [0x0a, 0x02, 0x61, 0x62]
        );
    }

    #[test]
    fn int_필드는_wire_type_0() {
        assert_eq!(
            encode_field(&field(2, FieldType::Int, json!(300))).unwrap(),
            [0x10, 0xac, 0x02]
        );
    }

    #[test]
    fn bool은_varint_0_1() {
        assert_eq!(
            encode_field(&field(3, FieldType::Bool, json!(true))).unwrap(),
            [0x18, 0x01]
        );
        assert_eq!(
            encode_field(&field(3, FieldType::Bool, json!(false))).unwrap(),
            [0x18, 0x00]
        );
    }

    #[test]
    fn bytes는_base64를_디코드해_넣는다() {
        assert_eq!(
            encode_field(&field(1, FieldType::Bytes, json!("QUI="))).unwrap(),
            [0x0a, 0x02, 0x41, 0x42]
        );
    }

    #[test]
    fn 두_string_필드를_이어_붙인다() {
        let buffer = encode_message(&[
            field(1, FieldType::String, json!("../x")),
            field(2, FieldType::String, json!("hi")),
        ])
        .unwrap();
        assert_eq!(
            buffer,
            [0x0a, 0x04, 0x2e, 0x2e, 0x2f, 0x78, 0x12, 0x02, 0x68, 0x69]
        );
    }

    #[test]
    fn 프레임을_붙이고_되돌린다() {
        let framed = frame_message(&[0xaa, 0xbb, 0xcc]);
        assert_eq!(framed.len(), 8);
        assert_eq!(framed[0], 0);
        assert_eq!(
            u32::from_be_bytes([framed[1], framed[2], framed[3], framed[4]]),
            3
        );
        assert_eq!(unframe_message(&framed), [0xaa, 0xbb, 0xcc]);
        // Under 5 bytes is an empty message.
        assert!(unframe_message(&[0, 0, 0]).is_empty());
    }

    #[tokio::test]
    async fn h2c로_요청을_보내고_트레일러의_grpc_status를_읽는다() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // A tiny gRPC-over-h2c server: echo the request's first string field back
        // as field 1, with grpc-status 0 in the trailers.
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut connection = h2::server::handshake(tcp).await.unwrap();
            // Keep accepting so the connection is driven until the client closes,
            // otherwise the response and trailers are not flushed.
            while let Some(request) = connection.accept().await {
                let (request, mut respond) = request.unwrap();
                let mut body = request.into_body();
                let mut received = Vec::new();
                while let Some(chunk) = body.data().await {
                    let chunk = chunk.unwrap();
                    let _ = body.flow_control().release_capacity(chunk.len());
                    received.extend_from_slice(&chunk);
                }
                let sent = extract_string_fields(&unframe_message(&received));
                let echo = sent.get(&1).cloned().unwrap_or_default();
                let response = http::Response::builder()
                    .status(200)
                    .header("content-type", "application/grpc")
                    .body(())
                    .unwrap();
                let mut stream = respond.send_response(response, false).unwrap();
                let reply = frame_message(
                    &encode_message(&[field(1, FieldType::String, json!(echo))]).unwrap(),
                );
                stream.send_data(reply.into(), false).unwrap();
                let mut trailers = http::HeaderMap::new();
                trailers.insert("grpc-status", "0".parse().unwrap());
                stream.send_trailers(trailers).unwrap();
            }
        });

        let message = encode_message(&[field(1, FieldType::String, json!("ping"))]).unwrap();
        let reply = send_grpc(
            &addr.to_string(),
            "/Svc/Echo",
            frame_message(&message),
            false,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!((reply.http_status, reply.grpc_status.as_str()), (200, "0"));
        let fields = extract_string_fields(&unframe_message(&reply.data));
        assert_eq!(fields.get(&1).map(String::as_str), Some("ping"));
    }

    #[test]
    fn 길이지정_필드만_utf8로_뽑고_varint는_건너뛴다() {
        let buffer = encode_message(&[
            field(1, FieldType::String, json!("submitted")),
            field(2, FieldType::Int, json!(7)),
            field(3, FieldType::String, json!("ok")),
        ])
        .unwrap();
        let got = extract_string_fields(&buffer);
        assert_eq!(got.get(&1).map(String::as_str), Some("submitted"));
        assert_eq!(got.get(&3).map(String::as_str), Some("ok"));
        assert_eq!(got.get(&2), None);
    }
}
