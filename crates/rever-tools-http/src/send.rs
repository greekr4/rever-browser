//! The single send path every probe funnels through, so the capture-not-send
//! safety net lives in one place. Mirrors the engine's `intercept::on_load`
//! fence rule: out-of-scope URLs are refused, and while the fence is active a
//! state-changing request is recorded instead of sent.

use std::time::{Duration, Instant};

use rever_scope::fence::{
    fence_reason, is_fence_active, is_lab_host, is_loopback_host, record_fence_block,
};
use rever_scope::ip_guard;
use rever_scope::scope::scope_block_for_url;

use crate::request_spec::RequestSpec;

/// Returned body is capped so a huge response can't blow up probe memory; the
/// probes only read status, headers, and a bounded slice of the body.
const MAX_BODY_BYTES: usize = 512 * 1024;

/// The outcome of one send. A request that never went out (scope refusal, fence
/// block, transport error) has `status` 0 (or 204 for a fence block) and says
/// why in `error`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SendOutcome {
    pub status: u16,
    /// Lowercased header names → values, as the TypeScript keyed them.
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

impl SendOutcome {
    fn refused(error: String) -> Self {
        SendOutcome {
            error: Some(error),
            ..SendOutcome::default()
        }
    }

    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(n, _)| *n == lower)
            .map(|(_, v)| v.as_str())
    }
}

/// Send one spec. Out of scope → refused; fence active and the request mutates
/// state → recorded and returned as 204, not sent. Every probe must go through
/// here so none can open its own socket and leak past the safety net.
pub async fn send(spec: &RequestSpec, timeout: Duration) -> SendOutcome {
    if let Some(block) = scope_block_for_url(Some(&spec.url)) {
        return SendOutcome::refused(block);
    }
    if is_fence_active()
        && let Some(reason) = fence_reason(&spec.method, &spec.url)
    {
        record_fence_block(&spec.method, &spec.url, reason);
        return SendOutcome {
            status: 204,
            error: Some(format!(
                "fence active: {reason:?} request recorded, not sent (capture-not-send). Mark the host a lab target to let it through."
            )),
            ..SendOutcome::default()
        };
    }
    // IP-level backstop, mirroring the page-load path in rever's intercept:
    // a scoped host that resolves only to forbidden addresses (DNS rebinding
    // into internal ranges or cloud metadata) must not be sent to either.
    // Loopback and lab hosts are exempt by name in fence_reason already.
    if is_fence_active()
        && !is_loopback_host(&spec.url)
        && !is_lab_host(&spec.url)
        && let Some(reason) = ip_guard::block_reason(&spec.url)
    {
        record_fence_block(&spec.method, &spec.url, reason);
        return SendOutcome {
            status: 204,
            error: Some(format!(
                "fence active: {reason:?} request recorded, not sent (host resolves only to forbidden addresses)"
            )),
            ..SendOutcome::default()
        };
    }
    send_unfenced(spec, timeout).await
}

/// The transport, with no scope/fence check — keep this private so the gate
/// above is the only entry point.
async fn send_unfenced(spec: &RequestSpec, timeout: Duration) -> SendOutcome {
    let started = Instant::now();
    let method = match reqwest::Method::from_bytes(spec.method.as_bytes()) {
        Ok(method) => method,
        Err(_) => return SendOutcome::refused(format!("invalid method: {}", spec.method)),
    };

    let mut header_map = reqwest::header::HeaderMap::new();
    for (name, value) in &spec.headers {
        // Skip a header reqwest can't represent rather than failing the whole
        // send — a probe tolerates a dropped header, not a dead request.
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            header_map.insert(name, value);
        }
    }

    // Targets are scoped / owned (loopback, lab, or an authorized host), where
    // self-signed certs are the norm — same rationale as raw_http's AcceptAnyCert.
    let client = match reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::limited(20))
        .timeout(timeout)
        .build()
    {
        Ok(client) => client,
        Err(error) => return SendOutcome::refused(error.to_string()),
    };

    let mut request = client.request(method, &spec.url).headers(header_map);
    if let Some(body) = &spec.body {
        request = request.body(body.clone());
    }

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            return SendOutcome {
                elapsed_ms: elapsed_ms(started),
                error: Some(error.to_string()),
                ..SendOutcome::default()
            };
        }
    };

    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            // Latin-1, as fetch keys header bytes.
            let value: String = value.as_bytes().iter().map(|&b| char::from(b)).collect();
            (name.as_str().to_ascii_lowercase(), value)
        })
        .collect();
    let body = match response.bytes().await {
        Ok(bytes) => {
            let slice = &bytes[..bytes.len().min(MAX_BODY_BYTES)];
            String::from_utf8_lossy(slice).into_owned()
        }
        Err(_) => String::new(),
    };

    SendOutcome {
        status,
        headers,
        body,
        elapsed_ms: elapsed_ms(started),
        error: None,
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_spec::RequestSpec;
    use std::sync::{Mutex, MutexGuard, PoisonError};

    fn spec(url: &str, method: &str) -> RequestSpec {
        RequestSpec {
            url: url.to_owned(),
            method: method.to_owned(),
            ..RequestSpec::default()
        }
    }

    /// Scope and fence are process-wide; cargo runs tests in parallel, so the
    /// ones touching that global state run one at a time and start clean.
    fn serial() -> MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        let guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        rever_scope::scope::clear_scope();
        while rever_scope::fence::is_fence_active() {
            rever_scope::fence::release_fence();
        }
        guard
    }

    #[tokio::test]
    #[expect(
        clippy::await_holding_lock,
        reason = "the guard keeps tests off the process-wide scope; a test runtime has one thread, so nothing else waits on it"
    )]
    async fn 스코프_밖_url은_전송하지_않고_거절한다() {
        let _serial = serial();
        rever_scope::scope::set_scope(&["allowed.test"]);
        let out = send(&spec("https://evil.test/x", "GET"), Duration::from_secs(1)).await;
        assert_eq!(out.status, 0);
        assert!(out.error.as_deref().unwrap_or("").contains("Out of scope"));
        rever_scope::scope::clear_scope();
    }

    #[tokio::test]
    #[expect(
        clippy::await_holding_lock,
        reason = "the guard keeps tests off the process-wide scope; a test runtime has one thread, so nothing else waits on it"
    )]
    async fn 펜스가_활성이면_변경_요청을_보내지_않고_204로_기록한다() {
        let _serial = serial();
        rever_scope::fence::acquire_fence();
        // A non-loopback POST is a mutation the fence must hold.
        let out = send(
            &spec("https://api.example.com/x", "POST"),
            Duration::from_secs(1),
        )
        .await;
        rever_scope::fence::release_fence();
        assert_eq!(out.status, 204);
        assert!(
            out.error
                .as_deref()
                .unwrap_or("")
                .contains("recorded, not sent")
        );
        assert!(
            rever_scope::fence::get_fence_blocks()
                .iter()
                .any(|b| b.url == "https://api.example.com/x")
        );
    }

    #[test]
    fn 헤더_조회는_대소문자를_무시한다() {
        let out = SendOutcome {
            headers: vec![("x-injected".to_owned(), "1".to_owned())],
            ..SendOutcome::default()
        };
        assert_eq!(out.header("X-Injected"), Some("1"));
        assert_eq!(out.header("missing"), None);
    }
}
