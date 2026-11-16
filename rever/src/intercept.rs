//! Request interception over Servo's resource load hook: the engine behind the
//! intercept and override tools (`rever_tools_debug::intercept::InterceptEngine`).
//!
//! Servo shows every HTTP request before it is sent and lets the app answer it
//! with a response of its own, cancel it, or let it go. It does not show the
//! request body or the real response, and a request cannot be changed on its way
//! out, so rules of the response stage and edits to a paused request are refused.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use rever_core::traffic;
use rever_tools_api::tool::{BoxFuture, unsupported};
use rever_tools_debug::intercept::{
    ContinueRequest, FulfillRequest, InterceptEngine, InterceptRule, InterceptState, Mode,
    PausedRequest, Stage,
};
use servo::{WebResourceLoad, WebResourceResponse};

use crate::bridge::{AppBrowser, Command};

/// The requests a modify rule holds, by the id the tools name them with.
pub type Held = Rc<RefCell<HashMap<String, WebResourceLoad>>>;

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
pub fn on_load(state: Arc<InterceptState>, held: Held) -> impl FnMut(WebResourceLoad) {
    let mut next_id = 0u64;
    move |load| {
        let url = load.request.url.to_string();
        // Dropping the load lets the request go to the network.
        let Some(rule) = state.matching_rule(&url) else {
            return;
        };
        match (rule.mode, rule.stage, rule.replace_body) {
            (_, Stage::Response, _) | (Mode::Log, ..) => {}
            (Mode::Block, ..) => answer(load, Decision::Fail),
            (Mode::Modify, _, Some(body)) => {
                let fulfill = Decision::Fulfill {
                    status: 200,
                    headers: rule.modify_headers.unwrap_or_default(),
                    body: body.into_bytes(),
                };
                answer(load, fulfill);
            }
            (Mode::Modify, _, None) => {
                next_id += 1;
                let request_id = format!("servo-{next_id}");
                let destination = format!("{:?}", load.request.destination);
                state.hold(PausedRequest {
                    request_id: request_id.clone(),
                    url,
                    method: load.request.method.to_string(),
                    resource_type: traffic::resource_type_of(&destination).to_owned(),
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
            for (name, value) in headers {
                // A header the agent mistyped is left out rather than failing the page load.
                let parsed = (
                    HeaderName::from_bytes(name.as_bytes()),
                    HeaderValue::from_str(&value),
                );
                if let (Ok(name), Ok(value)) = parsed {
                    map.append(name, value);
                }
            }
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
pub struct AppIntercept(pub Arc<AppBrowser>);

impl AppIntercept {
    fn decide(&self, request_id: &str, decision: Decision) -> BoxFuture<'_, Result<(), String>> {
        let request_id = request_id.to_owned();
        self.0
            .ask(|answer| Command::Decide(request_id, decision, answer))
    }
}

impl InterceptEngine for AppIntercept {
    /// The hook reads the rule list on every request, so there is nothing to
    /// switch on; a rule the engine cannot serve is refused here.
    fn apply_rules<'a>(&'a self, rules: &'a [InterceptRule]) -> BoxFuture<'a, Result<(), String>> {
        let unserved = rules.iter().any(|rule| rule.stage == Stage::Response);
        Box::pin(async move {
            match unserved {
                true => Err(unsupported("intercepting at the response stage")),
                false => Ok(()),
            }
        })
    }

    fn continue_request<'a>(
        &'a self,
        request: &'a ContinueRequest,
    ) -> BoxFuture<'a, Result<(), String>> {
        let edited = request.headers.is_some()
            || request.post_data.is_some()
            || request.url.is_some()
            || request.method.is_some();
        if edited {
            return Box::pin(async { Err(unsupported("changing a paused request")) });
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
