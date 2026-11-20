//! Request interception over Servo's resource load hook: the engine behind the
//! intercept and override tools (`rever_tools_debug::intercept::InterceptEngine`).
//!
//! Servo shows every HTTP request before it is sent and lets the app answer it
//! with a response of its own, cancel it, or let it go: that is the request
//! stage. The response stage, and changing the headers of a request that goes on
//! to the network, are in the patched `servo-net` (`net::tap::NetworkEditors`),
//! whose callbacks run on the network threads.
//!
//! Still out of reach: a held request's URL, method and body cannot be changed,
//! and a response's status cannot (its headers and body can).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use rever_core::traffic::{self, RequestPatch};
use rever_scope::{fence, scope};
use rever_tools_api::tool::{BoxFuture, unsupported};
use rever_tools_debug::intercept::{
    ContinueRequest, FulfillRequest, InterceptEngine, InterceptRule, InterceptState, Mode,
    PausedRequest, Stage,
};
use servo::{WebResourceLoad, WebResourceResponse};

use crate::bridge::{AppBrowser, Command};

/// The requests a modify rule holds, by the id the tools name them with.
pub type Held = Rc<RefCell<HashMap<String, WebResourceLoad>>>;

/// How long a response waits for the agent before it is delivered unchanged: the
/// network task that carries it waits with it.
const RESPONSE_HOLD: Duration = Duration::from_secs(60);

/// A request the agent let go with changed headers: its method, its URL, the headers.
type RequestEdit = (String, String, Vec<(String, String)>);

/// What the network threads and the tools share.
#[derive(Default)]
pub struct NetEdits {
    /// Header changes for a request the agent let go, by method and URL: the next
    /// such request to be sent takes them.
    requests: Mutex<Vec<RequestEdit>>,
    /// The responses whose body waits for the agent, by the id the tools use.
    responses: Mutex<HashMap<String, Sender<Decision>>>,
    next_response: AtomicU64,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn set_headers(map: &mut HeaderMap, headers: &[(String, String)]) {
    for (name, value) in headers {
        // A header the agent mistyped is left out rather than failing the load.
        let parsed = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        );
        if let (Ok(name), Ok(value)) = parsed {
            map.insert(name, value);
        }
    }
}

/// Install the response stage and the request header changes on the network
/// stack. Once, before the first page load.
pub fn install_editors(state: Arc<InterceptState>, edits: Arc<NetEdits>) {
    let response_rule = {
        let state = state.clone();
        move |url: &str| {
            state
                .matching_rule(url)
                .filter(|rule| rule.stage == Stage::Response)
        }
    };
    let (for_headers, for_body) = (response_rule.clone(), response_rule);
    let (request_edits, held) = (edits.clone(), edits);
    net::tap::set_network_editors(net::tap::NetworkEditors {
        request: Box::new(move |url, method, headers| {
            let mut waiting = locked(&request_edits.requests);
            let found = waiting
                .iter()
                .position(|(m, u, _)| m == method.as_str() && u == url.as_str());
            if let Some(at) = found {
                set_headers(headers, &waiting.remove(at).2);
            }
        }),
        response_headers: Box::new(move |url, headers| match for_headers(url.as_str()) {
            None => false,
            Some(rule) => match rule.mode {
                Mode::Log => false,
                Mode::Block => true,
                Mode::Modify => {
                    set_headers(headers, &rule.modify_headers.unwrap_or_default());
                    true
                }
            },
        }),
        response_body: Box::new(move |url, method, _status, _headers, body| {
            let Some(rule) = for_body(url.as_str()) else {
                return body;
            };
            match (rule.mode, rule.replace_body) {
                (Mode::Log, _) => body,
                // Too late to fail the load: the page gets an empty body.
                (Mode::Block, _) => Vec::new(),
                (Mode::Modify, Some(replacement)) => replacement.into_bytes(),
                (Mode::Modify, None) => {
                    let n = held.next_response.fetch_add(1, Ordering::Relaxed) + 1;
                    let request_id = format!("servo-r{n}");
                    let (decide, decided) = channel();
                    locked(&held.responses).insert(request_id.clone(), decide);
                    state.hold(PausedRequest {
                        request_id: request_id.clone(),
                        url: url.to_string(),
                        method: method.to_string(),
                        resource_type: "Other".to_owned(),
                    });
                    let decision = decided.recv_timeout(RESPONSE_HOLD);
                    locked(&held.responses).remove(&request_id);
                    state.release(&request_id);
                    match decision {
                        Ok(Decision::Fulfill { body, .. }) => body,
                        Ok(Decision::Fail) => Vec::new(),
                        Ok(Decision::Continue) | Err(_) => body,
                    }
                }
            }
        }),
    });
}

/// What the agent decided for a held request.
pub enum Decision {
    Continue,
    Fulfill {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    Fail,
}

/// Decides each request as Servo is about to send it: the first matching rule
/// blocks it, answers it with the rule's body, or holds it for the agent.
///
/// A request that is answered here never reaches the network tap, so `note`
/// records it: the capture shows what the page tried to send, with the status
/// it was answered with (none while it is held or when it was blocked).
pub fn on_load(
    state: Arc<InterceptState>,
    held: Held,
    note: impl Fn(RequestPatch),
) -> impl FnMut(WebResourceLoad) {
    let mut next_id = 0u64;
    move |load| {
        let url = load.request.url.to_string();
        let resource_type = {
            let destination = format!("{:?}", load.request.destination);
            traffic::resource_type_of(&destination).to_owned()
        };
        next_id += 1;
        let request_id = format!("servo-{next_id}");
        let noted = |status: Option<u16>| {
            let mut headers = traffic::Headers::new();
            for (name, value) in &load.request.headers {
                let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
                headers.insert(name.as_str().to_owned(), value);
            }
            note(RequestPatch {
                request_id: request_id.clone(),
                url: Some(url.clone()),
                method: Some(load.request.method.to_string()),
                resource_type: Some(resource_type.clone()),
                request_headers: Some(headers),
                status,
                ..RequestPatch::default()
            });
        };
        // The fence comes before any rule: while a crawl holds it, a request that
        // would change server state gets an empty 204 here and is only recorded.
        // As in the TypeScript, only for a request inside the scope that was set:
        // the crawl's own report draws the same line.
        if fence::is_fence_active()
            && scope::get_scope().enabled
            && scope::scope_block_for_url(Some(&url)).is_none()
            && let Some(reason) = fence::fence_reason(load.request.method.as_str(), &url)
        {
            fence::record_fence_block(load.request.method.as_str(), &url, reason);
            noted(Some(204));
            let held_back = Decision::Fulfill {
                status: 204,
                headers: Vec::new(),
                body: Vec::new(),
            };
            return answer(load, held_back);
        }
        // Dropping the load lets the request go to the network.
        let Some(rule) = state.matching_rule(&url) else {
            return;
        };
        match (rule.mode, rule.stage, rule.replace_body) {
            (_, Stage::Response, _) | (Mode::Log, ..) => {}
            (Mode::Block, ..) => {
                noted(None);
                answer(load, Decision::Fail);
            }
            (Mode::Modify, _, Some(body)) => {
                noted(Some(200));
                let fulfill = Decision::Fulfill {
                    status: 200,
                    headers: rule.modify_headers.unwrap_or_default(),
                    body: body.into_bytes(),
                };
                answer(load, fulfill);
            }
            (Mode::Modify, _, None) => {
                noted(None);
                state.hold(PausedRequest {
                    request_id: request_id.clone(),
                    url,
                    method: load.request.method.to_string(),
                    resource_type,
                });
                held.borrow_mut().insert(request_id, load);
            }
        }
    }
}

/// Carry out a decision on a request. Dropping it is what "continue" means.
pub fn answer(load: WebResourceLoad, decision: Decision) {
    let response = WebResourceResponse::new(load.request.url.clone().into_url());
    match decision {
        Decision::Continue => {}
        Decision::Fail => load.intercept(response).cancel(),
        Decision::Fulfill {
            status,
            headers,
            body,
        } => {
            let mut map = HeaderMap::new();
            set_headers(&mut map, &headers);
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            let reason = status.canonical_reason().unwrap_or_default();
            let response = response
                .headers(map)
                .status_code(status)
                .status_message(reason.as_bytes().to_vec());
            let mut intercepted = load.intercept(response);
            intercepted.send_body_data(body);
            intercepted.finish();
        }
    }
}

/// The tools' side: runs on any thread and asks the main thread, where the held
/// requests live.
pub struct AppIntercept {
    pub browser: Arc<AppBrowser>,
    pub edits: Arc<NetEdits>,
    /// To look a held request up by its id; weak, since the state owns this engine.
    pub state: Weak<InterceptState>,
}

impl AppIntercept {
    fn decide(&self, request_id: &str, decision: Decision) -> BoxFuture<'_, Result<(), String>> {
        // A held response waits on a network thread and is answered from here.
        if let Some(waiting) = locked(&self.edits.responses).remove(request_id) {
            let sent = waiting.send(decision);
            return Box::pin(async move {
                sent.map_err(|_| "the response is no longer waiting".to_owned())
            });
        }
        let request_id = request_id.to_owned();
        self.browser
            .ask(|answer| Command::Decide(request_id, decision, answer))
    }
}

impl InterceptEngine for AppIntercept {
    /// The hooks read the rule list on every request and response, so there is
    /// nothing to switch on.
    fn apply_rules<'a>(&'a self, _: &'a [InterceptRule]) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn continue_request<'a>(
        &'a self,
        request: &'a ContinueRequest,
    ) -> BoxFuture<'a, Result<(), String>> {
        if request.post_data.is_some() || request.url.is_some() || request.method.is_some() {
            let what = "changing a paused request's URL, method or body";
            return Box::pin(async move { Err(unsupported(what)) });
        }
        if let Some(headers) = &request.headers {
            // The request leaves under the same method and URL it was held with.
            let held = self.state.upgrade().and_then(|state| {
                let pending = state.pending();
                pending
                    .into_iter()
                    .find(|held| held.request_id == request.request_id)
            });
            if let Some(held) = held {
                let headers = headers
                    .iter()
                    .map(|header| (header.name.clone(), header.value.clone()))
                    .collect();
                locked(&self.edits.requests).push((held.method, held.url, headers));
            }
        }
        self.decide(&request.request_id, Decision::Continue)
    }

    fn fulfill_request<'a>(
        &'a self,
        request: &'a FulfillRequest,
    ) -> BoxFuture<'a, Result<(), String>> {
        let body = match request.body.as_deref().map(|body| BASE64.decode(body)) {
            Some(Ok(body)) => body,
            Some(Err(_)) => return Box::pin(async { Err("body is not base64".to_owned()) }),
            None => Vec::new(),
        };
        let headers = request.response_headers.iter().flatten();
        let fulfill = Decision::Fulfill {
            status: u16::try_from(request.response_code).unwrap_or(200),
            headers: headers
                .map(|header| (header.name.clone(), header.value.clone()))
                .collect(),
            body,
        };
        self.decide(&request.request_id, fulfill)
    }

    fn fail_request<'a>(
        &'a self,
        request_id: &'a str,
        _error_reason: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        self.decide(request_id, Decision::Fail)
    }
}
