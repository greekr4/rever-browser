//! Port of `src/main/mcp/tools/raw_http.ts` — send a raw HTTP/1.1 request over a
//! TCP (or TLS) socket, with the request line written exactly as given.
//!
//! This is a transport primitive, like Burp Repeater's raw mode: it carries no
//! payloads of its own. It is gated to loopback / lab hosts by the fence, so it
//! cannot be aimed at the wider internet without the host being marked an owned
//! lab target.

use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rever_scope::fence::{is_lab_host, is_loopback_host};
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Value, json};

use crate::{pretty, schema};

pub fn register(registry: &mut ToolRegistry) {
    registry.register(RawHttp);
}

/// Build an HTTP/1.1 request text. The request line is written verbatim — no URL
/// normalisation, no fragment stripping — so a target like `/x#/healthcheck-dev`
/// reaches the server intact (CVE-2023-45539 and the like).
pub fn build_raw_request(
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    host: &str,
) -> String {
    let lower: Vec<String> = headers
        .iter()
        .map(|(name, _)| name.to_lowercase())
        .collect();
    let has = |name: &str| lower.iter().any(|header| header == name);
    let mut lines = vec![format!("{method} {path} HTTP/1.1")];
    if !has("host") {
        lines.push(format!("Host: {host}"));
    }
    for (name, value) in headers {
        lines.push(format!("{name}: {value}"));
    }
    if !has("connection") {
        lines.push("Connection: close".to_owned());
    }
    if let Some(body) = body
        && !has("content-length")
    {
        lines.push(format!("Content-Length: {}", body.len()));
    }
    format!("{}\r\n\r\n{}", lines.join("\r\n"), body.unwrap_or(""))
}

/// A raw HTTP response split into its parts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawResponse {
    pub status_line: String,
    pub status: u16,
    /// Lowercased header names → values, as the TypeScript keyed them.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// Split a raw HTTP response into status line, headers, and body. Bytes are read
/// as Latin-1 (each byte one char), as the TypeScript did.
pub fn parse_raw_response(raw: &[u8]) -> RawResponse {
    let latin1 = |bytes: &[u8]| bytes.iter().map(|&byte| byte as char).collect::<String>();
    let sep = raw.windows(4).position(|window| window == b"\r\n\r\n");
    let (head, body) = match sep {
        Some(at) => (latin1(&raw[..at]), latin1(&raw[at + 4..])),
        None => (latin1(raw), String::new()),
    };
    let mut parts = head.split("\r\n");
    let status_line = parts.next().unwrap_or("").to_owned();
    let headers = parts
        .filter_map(|line| {
            let at = line.find(':').filter(|at| *at > 0)?;
            Some((
                line[..at].trim().to_lowercase(),
                line[at + 1..].trim().to_owned(),
            ))
        })
        .collect();
    // `Number(statusLine.split(' ')[1]) || 0`.
    let status = status_line
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    RawResponse {
        status_line,
        status,
        headers,
        body,
    }
}

/// Accepts any server certificate, like the TypeScript's `rejectUnauthorized: false`.
/// Sound here: `raw_http` only reaches loopback / lab hosts, where self-signed
/// certificates are the norm and there is no trust decision to protect.
#[derive(Debug)]
pub(crate) struct AcceptAnyCert(pub(crate) Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Read `socket` until it closes or `timeout` elapses, returning whatever
/// arrived. A short response that never sends FIN comes back at the timeout.
fn read_until_close(socket: &mut impl Read_, timeout: Duration) -> Vec<u8> {
    let deadline = std::time::Instant::now() + timeout;
    let mut out = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        socket.set_read_timeout(Some(left));
        match socket.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buffer[..n]),
            // A read timeout (WouldBlock/TimedOut) just ends the collection.
            Err(_) => break,
        }
    }
    out
}

/// A socket this module reads with a per-read timeout: a plain `TcpStream` or a
/// rustls stream over one.
trait Read_ {
    fn set_read_timeout(&mut self, timeout: Option<Duration>);
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize>;
}

impl Read_ for TcpStream {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        let _ = TcpStream::set_read_timeout(self, timeout);
    }
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(self, buffer)
    }
}

struct TlsSocket<'a> {
    tls: rustls::Stream<'a, rustls::ClientConnection, TcpStream>,
}

impl Read_ for TlsSocket<'_> {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.tls.sock.set_read_timeout(timeout);
    }
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.tls.read(buffer)
    }
}

/// Open a socket to `host:port`, write `payload` verbatim, and collect the raw
/// response bytes. Public so probes that must control the `Cookie` header
/// (idor/noauth) can replay over a raw socket — fetch forbids that header.
pub fn send_raw_over_socket(
    host: &str,
    port: u16,
    payload: &[u8],
    tls: bool,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let address = (host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or_else(|| format!("could not resolve {host}"))?;
    let mut socket = TcpStream::connect_timeout(&address, timeout).map_err(|e| e.to_string())?;
    if !tls {
        socket.write_all(payload).map_err(|e| e.to_string())?;
        return Ok(read_until_close(&mut socket, timeout));
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
        .with_no_client_auth();
    let server_name = rustls_pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| format!("invalid TLS server name: {host}"))?;
    let mut conn =
        rustls::ClientConnection::new(Arc::new(config), server_name).map_err(|e| e.to_string())?;
    let mut stream = rustls::Stream::new(&mut conn, &mut socket);
    stream.write_all(payload).map_err(|e| e.to_string())?;
    let mut wrapped = TlsSocket { tls: stream };
    Ok(read_until_close(&mut wrapped, timeout))
}

pub struct RawHttp;

impl Tool for RawHttp {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "raw_http",
            "Send a raw HTTP/1.1 request over a TCP socket with the request line written EXACTLY as given — no URL normalisation, no fragment stripping. Use to reach path-confused routes (e.g. \"/x#/healthcheck-dev\") a normal client would rewrite. Gated to loopback / lab hosts by the capture-not-send fence.",
            schema(
                json!({
                    "host": { "type": "string", "description": "Target host, e.g. \"127.0.0.1\"" },
                    "port": { "type": "integer", "description": "Target port, e.g. 1337" },
                    "method": { "type": "string", "description": "e.g. GET (ignored if raw is set)" },
                    "path": { "type": "string", "description": "Request target sent verbatim, e.g. \"/x#/healthcheck-dev\" (ignored if raw is set)" },
                    "headers": { "type": "object", "description": "Extra request headers" },
                    "body": { "type": "string" },
                    "raw": { "type": "string", "description": "A complete raw request to send byte-for-byte (use \\r\\n line endings); overrides method/path/headers/body" },
                    "tls": { "type": "boolean", "description": "Wrap in TLS (default false)" },
                    "timeoutMs": { "type": "integer", "description": "default 8000" }
                }),
                &["host", "port"],
            ),
        )
    }

    fn call<'a>(&'a self, _ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let Some(host) = args["host"].as_str().map(str::to_owned) else {
                return err("host is required");
            };
            let Some(port) = args["port"].as_u64().and_then(|p| u16::try_from(p).ok()) else {
                return err("port is required");
            };
            // Only loopback / lab targets, as the fence allows.
            let url = format!(
                "{}://{host}:{port}/",
                if args["tls"].as_bool().unwrap_or(false) {
                    "https"
                } else {
                    "http"
                }
            );
            if !is_loopback_host(&url) && !is_lab_host(&url) {
                return err(format!(
                    "raw_http is gated to loopback / lab hosts (fence policy); {host} is neither. Mark it as a lab target first if you own it."
                ));
            }
            let tls = args["tls"].as_bool().unwrap_or(false);
            let timeout = Duration::from_millis(args["timeoutMs"].as_u64().unwrap_or(8000));
            let payload = match args["raw"].as_str() {
                Some(raw) => raw.to_owned(),
                None => build_raw_request(
                    args["method"].as_str().unwrap_or("GET"),
                    args["path"].as_str().unwrap_or("/"),
                    &header_pairs(&args["headers"]),
                    args["body"].as_str(),
                    &format!("{host}:{port}"),
                ),
            };
            // Latin-1 encode the payload: each char 0..=255 is one byte.
            let bytes: Vec<u8> = payload.chars().map(|c| c as u8).collect();
            let result = match send_raw_over_socket(&host, port, &bytes, tls, timeout) {
                Ok(result) => result,
                Err(error) => return err(error),
            };
            if result.is_empty() {
                return err("no response (connection made, nothing returned before timeout)");
            }
            let parsed = parse_raw_response(&result);
            let headers: serde_json::Map<String, Value> = parsed
                .headers
                .iter()
                .map(|(name, value)| (name.clone(), json!(value)))
                .collect();
            let body_bytes = parsed.body.chars().count();
            ok(pretty(&json!({
                "sent": payload.split("\r\n").next().unwrap_or_default(),
                "statusLine": parsed.status_line,
                "status": parsed.status,
                "headers": headers,
                "bodyPreview": parsed.body.chars().take(1200).collect::<String>(),
                "bodyBytes": body_bytes,
            })))
        })
    }
}

/// Header object → (name, value) pairs in the order given.
fn header_pairs(value: &Value) -> Vec<(String, String)> {
    value
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn 요청_라인을_그대로_쓰고_fragment를_지우지_않는다() {
        let request = build_raw_request("GET", "/x#/healthcheck-dev", &[], None, "127.0.0.1:1337");
        assert_eq!(
            request.split("\r\n").next(),
            Some("GET /x#/healthcheck-dev HTTP/1.1")
        );
        assert!(request.contains("Host: 127.0.0.1:1337"));
        assert!(request.contains("Connection: close"));
        assert!(request.ends_with("\r\n\r\n"));
    }

    #[test]
    fn body가_있으면_content_length를_자동으로_계산한다() {
        let request = build_raw_request(
            "POST",
            "/x",
            &headers(&[("Content-Type", "application/json")]),
            Some("{\"a\":1}"),
            "h:80",
        );
        assert!(request.contains("Content-Length: 7"));
        assert!(request.contains("Content-Type: application/json"));
        assert!(request.ends_with("\r\n\r\n{\"a\":1}"));
    }

    #[test]
    fn 사용자가_준_host_connection_content_length는_중복_추가하지_않는다() {
        let request = build_raw_request(
            "GET",
            "/",
            &headers(&[("Host", "evil"), ("Connection", "keep-alive")]),
            None,
            "real:80",
        );
        assert!(request.contains("Host: evil"));
        assert!(!request.contains("Host: real:80"));
        assert_eq!(request.matches("Connection:").count(), 1);
    }

    #[test]
    fn status_line_헤더_바디를_분리한다() {
        let raw =
            b"HTTP/1.1 403 Forbidden\r\nContent-Type: text/html\r\nContent-Length: 2\r\n\r\nno";
        let parsed = parse_raw_response(raw);
        assert_eq!(parsed.status_line, "HTTP/1.1 403 Forbidden");
        assert_eq!(parsed.status, 403);
        assert_eq!(
            parsed.headers.iter().find(|(n, _)| n == "content-type"),
            Some(&("content-type".to_owned(), "text/html".to_owned()))
        );
        assert_eq!(parsed.body, "no");
    }

    #[test]
    fn 바디_없는_응답() {
        let parsed = parse_raw_response(b"HTTP/1.1 200 OK\r\nServer: x\r\n\r\n");
        assert_eq!(parsed.status, 200);
        assert_eq!(
            parsed.headers.iter().find(|(n, _)| n == "server"),
            Some(&("server".to_owned(), "x".to_owned()))
        );
        assert_eq!(parsed.body, "");
    }
}
