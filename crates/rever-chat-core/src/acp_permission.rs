//! Port of `src/renderer/src/ai/acp-permission.ts` — the queue of agent permission prompts.
//!
//! The TypeScript was two zustand stores plus module functions; here it is the
//! [`PermissionStore`] struct. What the port had to give a Rust shape:
//!
//! - A request is the ACP `RequestPermissionRequest` JSON object, as rever-agent
//!   emits it (`{ sessionId, toolCall: { toolCallId, title, rawInput }, options }`).
//! - The promise's `resolve` is a callback the caller passes with the request. It
//!   is called from inside the store's methods, so it must not call back into the
//!   store (send the answer over a channel instead).
//! - The `setTimeout` is the caller's: a queued request returns a
//!   [`PermissionId`], and the host calls [`PermissionStore::expire`] with it
//!   after [`ACP_PERMISSION_TIMEOUT_MS`]. Expiring an id that was already
//!   answered does nothing, which is what `clearTimeout` did.
//! - `src/shared/risky-tools.ts` is ported in another crate, so its decisions
//!   come in through the [`RiskyTools`] trait.
//! - `recordApprovalAudit` (stores/approval-audit.ts) is the audit callback given
//!   to [`PermissionStore::new`]; the Red Team state (red_team_mode.rs) is lent to
//!   each request, and the session grants (session_grants.rs) live in the store.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RiskyTools;
use crate::red_team_mode::RedTeamMode;
use crate::session_grants::SessionGrants;

/// `ACP_PERMISSION_TIMEOUT_MS` from the renderer's constants.ts: an unanswered
/// prompt is rejected after this long.
pub const ACP_PERMISSION_TIMEOUT_MS: u64 = 60_000;

/// The localStorage key the auto-approve setting was persisted under. Persisting
/// is the host's job; the store only holds the value (default on).
pub const AUTO_APPROVE_STORAGE_KEY: &str = "rever-browser:acp-auto-approve";

/// ACP `RequestPermissionResponse`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestPermissionResponse {
    pub outcome: RequestPermissionOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "outcome",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RequestPermissionOutcome {
    Cancelled,
    Selected { option_id: String },
}

fn selected(option_id: String) -> RequestPermissionResponse {
    RequestPermissionResponse {
        outcome: RequestPermissionOutcome::Selected { option_id },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Reject,
}

/// What `recordApprovalAudit` received: an audit entry before the audit store
/// adds its id and timestamp.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalAudit {
    /// Best-effort tool label (matched risky tool id, else the request title).
    pub tool: String,
    /// Target host extracted from the tool input, if any.
    pub target: Option<String>,
    pub decision: Decision,
    /// True when resolved without the user answering the prompt.
    pub auto: bool,
    pub risky: bool,
}

type Resolve = Box<dyn FnOnce(RequestPermissionResponse) + Send>;
type Audit = Box<dyn FnMut(ApprovalAudit) + Send>;

/// Names a queued request for [`PermissionStore::expire`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PermissionId(u64);

pub struct PendingPermission {
    id: PermissionId,
    request: Value,
    resolve: Resolve,
}

impl PendingPermission {
    pub fn id(&self) -> PermissionId {
        self.id
    }

    pub fn request(&self) -> &Value {
        &self.request
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApproveOptions {
    /// Also allow every later call of this gated tool in the same session.
    pub grant_session: bool,
}

pub struct PermissionStore<R> {
    queue: Vec<PendingPermission>,
    auto_approve: bool,
    grants: SessionGrants,
    risky: R,
    audit: Audit,
    next_id: u64,
}

fn session_id_of(request: &Value) -> Option<&str> {
    request.get("sessionId").and_then(Value::as_str)
}

fn raw_input_of(request: &Value) -> Option<&Value> {
    request.pointer("/toolCall/rawInput")
}

fn decision_for(request: &Value, option_id: &str) -> Decision {
    let kind = request
        .get("options")
        .and_then(Value::as_array)
        .and_then(|options| {
            options
                .iter()
                .find(|o| o.get("optionId").and_then(Value::as_str) == Some(option_id))
        })
        .and_then(|o| o.get("kind"))
        .and_then(Value::as_str);
    if kind.is_some_and(|kind| kind.starts_with("reject")) {
        Decision::Reject
    } else {
        Decision::Allow
    }
}

impl<R: RiskyTools> PermissionStore<R> {
    pub fn new(risky: R, audit: impl FnMut(ApprovalAudit) + Send + 'static) -> Self {
        Self {
            queue: Vec::new(),
            auto_approve: true,
            grants: SessionGrants::new(),
            risky,
            audit: Box::new(audit),
            next_id: 0,
        }
    }

    /// The prompt to show: the head of the queue.
    pub fn current_permission(&self) -> Option<&PendingPermission> {
        self.queue.first()
    }

    pub fn permission_queue(&self) -> &[PendingPermission] {
        &self.queue
    }

    pub fn acp_auto_approve(&self) -> bool {
        self.auto_approve
    }

    pub fn set_acp_auto_approve(&mut self, v: bool) {
        self.auto_approve = v;
    }

    pub fn session_grants(&self) -> &SessionGrants {
        &self.grants
    }

    pub fn session_grants_mut(&mut self) -> &mut SessionGrants {
        &mut self.grants
    }

    fn record(&mut self, request: &Value, option_id: &str, auto: bool, risky: bool) {
        let title = request
            .pointer("/toolCall/title")
            .and_then(Value::as_str)
            .map(str::to_string);
        let entry = ApprovalAudit {
            tool: self
                .risky
                .matched_risky_tool(request)
                .or(title)
                .unwrap_or_else(|| "tool".to_string()),
            target: self.risky.extract_target_host(raw_input_of(request)),
            decision: decision_for(request, option_id),
            auto,
            risky,
        };
        (self.audit)(entry);
    }

    fn auto_allow(&mut self, params: &Value, risky: bool, resolve: Resolve) {
        let option_id = self.risky.choose_allow_option(params);
        self.record(params, &option_id, true, risky);
        resolve(selected(option_id));
    }

    /// Answers `params` at once when a rule covers it (then `resolve` has already
    /// run and this returns `None`), or queues it for the user and returns its id:
    /// the caller then arms a timer of [`ACP_PERMISSION_TIMEOUT_MS`] that calls
    /// [`Self::expire`].
    pub fn request_permission_from_user(
        &mut self,
        params: Value,
        red_team: &RedTeamMode,
        resolve: impl FnOnce(RequestPermissionResponse) + Send + 'static,
    ) -> Option<PermissionId> {
        let resolve: Resolve = Box::new(resolve);
        let risky = self.risky.is_risky_tool_request(&params);
        let matched = self.risky.matched_risky_tool(&params);

        // Red Team scan: auto-approve probe tools (even risky ones) on the scan's own
        // scoped site — the user authorized the sweep of their own target.
        // - Raw shell HTTP (curl/wget in Bash) is excluded: it runs in the agent
        //   process and bypasses the server scope check, so the scan's own pass
        //   does not cover it. It follows the auto-approve setting like any
        //   other call.
        // - A known host that is out of scope falls through to the normal prompt.
        // - No resolvable host (replay_request/repeater_send target a captured request
        //   by id) → auto-approve: the server-side scope check (repeater.ts →
        //   scopeBlockForUrl) already blocks replays to out-of-scope URLs.
        if red_team.is_red_team_active() && matched.as_deref() != Some(self.risky.shell_http_tool())
        {
            let host = self.risky.extract_target_host(raw_input_of(&params));
            if host
                .as_deref()
                .is_none_or(|host| red_team.host_in_red_team_scope(Some(host)))
            {
                self.auto_allow(&params, risky, resolve);
                return None;
            }
        }

        // Auto-approve answers everything, risky or not: the setting is the
        // switch. The audit still marks what was risky.
        if self.auto_approve {
            self.auto_allow(&params, risky, resolve);
            return None;
        }

        // The user already chose "allow all this session" for this gated tool.
        // allow_once still — the grant lives here, so every call is audited.
        if self.grants.has_session_grant(&params, &self.risky) {
            self.auto_allow(&params, risky, resolve);
            return None;
        }

        let id = PermissionId(self.next_id);
        self.next_id += 1;
        self.queue.push(PendingPermission {
            id,
            request: params,
            resolve,
        });
        Some(id)
    }

    /// The timeout of a queued request: rejects it if it is still waiting.
    pub fn expire(&mut self, id: PermissionId) {
        let Some(at) = self.queue.iter().position(|entry| entry.id == id) else {
            return;
        };
        let entry = self.queue.remove(at);
        let option_id = self.risky.find_reject_option(&entry.request);
        let risky = self.risky.is_risky_tool_request(&entry.request);
        self.record(&entry.request, &option_id, false, risky);
        (entry.resolve)(selected(option_id));
    }

    /// Takes the head of the queue and answers it with `pick`'s option. Returns
    /// the request and the option, for the session grant that may follow.
    fn decide(&mut self, pick: impl FnOnce(&R, &Value) -> String) -> Option<(Value, String)> {
        if self.queue.is_empty() {
            return None;
        }
        let PendingPermission {
            request, resolve, ..
        } = self.queue.remove(0);
        let option_id = pick(&self.risky, &request);
        let risky = self.risky.is_risky_tool_request(&request);
        self.record(&request, &option_id, false, risky);
        resolve(selected(option_id.clone()));
        Some((request, option_id))
    }

    /// After a session grant, calls of that tool already waiting in the queue are
    /// covered by it too — resolve them instead of prompting one by one.
    fn apply_session_grant(&mut self, request: &Value) {
        self.grants.grant_session(request, &self.risky);
        for queued in std::mem::take(&mut self.queue) {
            if !self.grants.has_session_grant(&queued.request, &self.risky) {
                self.queue.push(queued);
                continue;
            }
            let option_id = self.risky.choose_allow_option(&queued.request);
            let risky = self.risky.is_risky_tool_request(&queued.request);
            self.record(&queued.request, &option_id, true, risky);
            (queued.resolve)(selected(option_id));
        }
    }

    pub fn respond_to_permission(&mut self, picked: &str, opts: ApproveOptions) {
        let Some((request, option_id)) =
            self.decide(|risky, request| risky.sanitize_selection(request, picked))
        else {
            return;
        };
        if opts.grant_session && decision_for(&request, &option_id) == Decision::Allow {
            self.apply_session_grant(&request);
        }
    }

    pub fn approve_current_permission(&mut self, opts: ApproveOptions) {
        let Some((request, _)) = self.decide(|risky, request| risky.choose_allow_option(request))
        else {
            return;
        };
        if opts.grant_session {
            self.apply_session_grant(&request);
        }
    }

    pub fn reject_current_permission(&mut self) {
        self.decide(|risky, request| risky.find_reject_option(request));
    }

    pub fn has_pending_permission(&self, session_id: &str) -> bool {
        self.queue
            .iter()
            .any(|entry| session_id_of(&entry.request) == Some(session_id))
    }

    /// Stop: ACP requires the client to answer every pending permission request of
    /// a cancelled turn with the `cancelled` outcome. Without this the stale prompt
    /// stays queued and pops up again in front of the next turn's prompt.
    pub fn cancel_pending_permissions(&mut self, session_id: &str) {
        for entry in std::mem::take(&mut self.queue) {
            if session_id_of(&entry.request) != Some(session_id) {
                self.queue.push(entry);
                continue;
            }
            let option_id = self.risky.find_reject_option(&entry.request);
            let risky = self.risky.is_risky_tool_request(&entry.request);
            self.record(&entry.request, &option_id, true, risky);
            (entry.resolve)(RequestPermissionResponse {
                outcome: RequestPermissionOutcome::Cancelled,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use serde_json::json;

    use super::*;
    use crate::test_risky_tools::TsRiskyTools;

    type Shared<T> = Arc<Mutex<T>>;

    fn read<T: Clone>(shared: &Shared<T>) -> T {
        shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// One test's store: the Red Team state the TypeScript kept in a module, the
    /// audit trail, and a request helper that hands back the "promise".
    /// Auto-approve is off in it, the mode that has prompts. A test turns it on
    /// to assert what it skips.
    struct Fixture {
        store: PermissionStore<TsRiskyTools>,
        red_team: RedTeamMode,
        audits: Shared<Vec<ApprovalAudit>>,
    }

    impl Fixture {
        fn new() -> Self {
            let audits: Shared<Vec<ApprovalAudit>> = Shared::default();
            let sink = audits.clone();
            let mut store = PermissionStore::new(TsRiskyTools, move |entry| {
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(entry)
            });
            store.set_acp_auto_approve(false);
            Self {
                store,
                red_team: RedTeamMode::new(),
                audits,
            }
        }

        /// The answer slot fills when the request resolves, like awaiting the promise.
        fn request(&mut self, params: Value) -> (Option<PermissionId>, Shared<Option<Value>>) {
            let answer: Shared<Option<Value>> = Shared::default();
            let slot = answer.clone();
            let id =
                self.store
                    .request_permission_from_user(params, &self.red_team, move |response| {
                        *slot.lock().unwrap_or_else(PoisonError::into_inner) =
                            serde_json::to_value(response).ok();
                    });
            (id, answer)
        }
    }

    fn tool_req(session_id: &str, title: &str, raw_input: Value) -> Value {
        json!({
            "sessionId": session_id,
            "options": [
                { "kind": "allow_once", "name": "Allow", "optionId": "allow" },
                { "kind": "reject_once", "name": "Reject", "optionId": "reject" }
            ],
            "toolCall": { "toolCallId": "t", "title": title, "rawInput": raw_input }
        })
    }

    fn risky_req(session_id: &str) -> Value {
        tool_req(session_id, "mcp__rever-traffic__burst_send", json!({}))
    }

    fn allow() -> Option<Value> {
        Some(json!({ "outcome": { "outcome": "selected", "optionId": "allow" } }))
    }

    fn cancelled() -> Option<Value> {
        Some(json!({ "outcome": { "outcome": "cancelled" } }))
    }

    #[test]
    fn stop한_세션의_대기_중_권한_요청을_cancelled로_응답하고_큐에서_뺀다() {
        let mut f = Fixture::new();
        let (_, pending) = f.request(risky_req("s1"));
        assert!(f.store.has_pending_permission("s1"));

        f.store.cancel_pending_permissions("s1");

        assert_eq!(read(&pending), cancelled());
        assert!(!f.store.has_pending_permission("s1"));
    }

    #[test]
    fn 다른_세션의_요청은_건드리지_않는다() {
        let mut f = Fixture::new();
        let (_, other) = f.request(risky_req("s2"));
        f.store.cancel_pending_permissions("s1");
        assert!(f.store.has_pending_permission("s2"));
        f.store.cancel_pending_permissions("s2");
        assert_eq!(read(&other), cancelled());
    }

    #[test]
    fn red_team_범위_내_호스트의_위험_probe를_자동승인한다() {
        let mut f = Fixture::new();
        f.red_team
            .set_red_team_scan(true, Some("http://localhost:8080/my"));
        let (_, r) = f.request(tool_req(
            "rt1",
            "mcp__rever-traffic__path_probe",
            json!({ "baseUrl": "http://localhost:8080/" }),
        ));
        assert_eq!(read(&r), allow());
        assert!(!f.store.has_pending_permission("rt1"));
    }

    #[test]
    fn red_team_host를_못_뽑는_replay_request도_자동승인한다_서버_scope가_막음() {
        let mut f = Fixture::new();
        f.red_team
            .set_red_team_scan(true, Some("http://localhost:8080/my"));
        let (_, r) = f.request(tool_req(
            "rt2",
            "mcp__rever-traffic__replay_request",
            json!({ "requestId": "27237.71" }),
        ));
        assert_eq!(read(&r), allow());
    }

    #[test]
    fn red_team_범위_밖_포트_8090은_자동승인하지_않고_프롬프트한다() {
        let mut f = Fixture::new();
        f.red_team
            .set_red_team_scan(true, Some("http://localhost:8080/my"));
        f.request(tool_req(
            "rt3",
            "mcp__rever-traffic__path_probe",
            json!({ "baseUrl": "http://localhost:8090/" }),
        ));
        assert!(f.store.has_pending_permission("rt3"));
        f.store.cancel_pending_permissions("rt3");
    }

    #[test]
    fn red_team_scope_add된_api_호스트_3000의_replay는_자동승인한다() {
        let mut f = Fixture::new();
        f.red_team
            .set_red_team_scan(true, Some("http://localhost:8080/my"));
        f.red_team.add_red_team_scope(&["http://localhost:3000/"]);
        let (_, r) = f.request(tool_req(
            "rt5",
            "mcp__rever-traffic__replay_request",
            json!({ "requestId": "1", "url": "http://localhost:3000/v1/users/1" }),
        ));
        assert_eq!(read(&r), allow());
    }

    #[test]
    fn red_team_셸_http_curl는_red_team이어도_프롬프트한다() {
        let mut f = Fixture::new();
        f.red_team
            .set_red_team_scan(true, Some("http://localhost:8080/my"));
        f.request(tool_req(
            "rt4",
            "Bash",
            json!({ "command": "curl http://localhost:8080/x" }),
        ));
        assert!(f.store.has_pending_permission("rt4"));
        f.store.cancel_pending_permissions("rt4");
    }

    // The cases below are not in the TypeScript test file: they cover the paths
    // it leaves out (auto-approve, timeout, user answers, session grants, audit).

    fn full_req(session_id: &str, title: &str, raw_input: Value) -> Value {
        let mut req = tool_req(session_id, title, raw_input);
        req["options"] = json!([
            { "kind": "allow_always", "name": "Always", "optionId": "always" },
            { "kind": "allow_once", "name": "Allow", "optionId": "allow" },
            { "kind": "reject_once", "name": "Reject", "optionId": "reject" }
        ]);
        req
    }

    fn selected_json(option_id: &str) -> Option<Value> {
        Some(json!({ "outcome": { "outcome": "selected", "optionId": option_id } }))
    }

    #[test]
    fn 저장소는_자동_승인이_켜진_채로_시작한다() {
        assert!(PermissionStore::new(TsRiskyTools, |_| {}).acp_auto_approve());
    }

    #[test]
    fn 자동_승인이_켜지면_위험_도구도_묻지_않고_허용하고_감사에는_위험으로_남긴다() {
        let mut f = Fixture::new();
        f.store.set_acp_auto_approve(true);
        let (id, r) = f.request(risky_req("s1"));
        assert_eq!(id, None);
        assert_eq!(read(&r), allow());
        assert!(!f.store.has_pending_permission("s1"));
        let audits = read(&f.audits);
        assert_eq!(audits.len(), 1);
        assert_eq!(
            (audits[0].decision, audits[0].auto, audits[0].risky),
            (Decision::Allow, true, true)
        );
    }

    #[test]
    fn 위험하지_않은_툴은_자동승인_설정에_따라_바로_허용하거나_큐에_넣는다() {
        let mut f = Fixture::new();
        f.store.set_acp_auto_approve(true);
        let (id, r) = f.request(full_req("s1", "Read", json!({ "file_path": "/a" })));
        assert_eq!(id, None);
        assert_eq!(read(&r), selected_json("always"));
        assert_eq!(
            read(&f.audits),
            [ApprovalAudit {
                tool: "Read".to_string(),
                target: None,
                decision: Decision::Allow,
                auto: true,
                risky: false,
            }]
        );

        f.store.set_acp_auto_approve(false);
        let (id, r) = f.request(full_req("s1", "Read", json!({})));
        assert!(id.is_some());
        assert_eq!(read(&r), None);
        assert_eq!(
            f.store.current_permission().map(|entry| (
                Some(entry.id()),
                entry.request()["toolCall"]["title"].clone()
            )),
            Some((id, json!("Read")))
        );
        assert_eq!(f.store.permission_queue().len(), 1);
    }

    #[test]
    fn 시간이_지나면_거절로_응답하고_이미_답한_요청의_만료는_무시한다() {
        let mut f = Fixture::new();
        let (first, a) = f.request(risky_req("s1"));
        let (second, b) = f.request(risky_req("s1"));
        let (Some(first), Some(second)) = (first, second) else {
            panic!("risky requests must queue");
        };

        f.store.expire(second);
        assert_eq!(read(&b), selected_json("reject"));
        assert_eq!(read(&a), None);
        assert_eq!(
            read(&f.audits),
            [ApprovalAudit {
                tool: "burst_send".to_string(),
                target: None,
                decision: Decision::Reject,
                auto: false,
                risky: true,
            }]
        );

        f.store
            .approve_current_permission(ApproveOptions::default());
        assert_eq!(read(&a), allow());
        f.store.expire(first);
        f.store.expire(second);
        assert_eq!(read(&f.audits).len(), 2);
        assert!(!f.store.has_pending_permission("s1"));
    }

    #[test]
    fn 사용자_응답은_큐의_맨_앞_요청에만_적용된다() {
        let mut f = Fixture::new();
        // An empty queue ignores every answer.
        f.store.reject_current_permission();
        f.store
            .approve_current_permission(ApproveOptions::default());
        f.store
            .respond_to_permission("allow", ApproveOptions::default());
        assert!(read(&f.audits).is_empty());

        let (_, a) = f.request(risky_req("s1"));
        let (_, b) = f.request(full_req(
            "s2",
            "mcp__rever-traffic__path_probe",
            json!({ "baseUrl": "https://api.example.com:8443/" }),
        ));
        f.store.reject_current_permission();
        assert_eq!(read(&a), selected_json("reject"));
        assert_eq!(read(&b), None);

        // "Always allow" on a gated tool is downgraded to allow_once.
        f.store
            .respond_to_permission("always", ApproveOptions::default());
        assert_eq!(read(&b), allow());
        assert_eq!(
            read(&f.audits).last(),
            Some(&ApprovalAudit {
                tool: "path_probe".to_string(),
                target: Some("api.example.com:8443".to_string()),
                decision: Decision::Allow,
                auto: false,
                risky: true,
            })
        );
    }

    #[test]
    fn 세션_허용은_대기_중인_같은_툴_호출과_이후_호출을_함께_푼다() {
        let mut f = Fixture::new();
        let (_, first) = f.request(risky_req("s1"));
        let (_, same_tool) = f.request(risky_req("s1"));
        let (_, other_tool) = f.request(tool_req(
            "s1",
            "mcp__rever-traffic__intruder_run",
            json!({}),
        ));
        let (_, other_session) = f.request(risky_req("s2"));

        f.store.approve_current_permission(ApproveOptions {
            grant_session: true,
        });

        assert_eq!(read(&first), allow());
        assert_eq!(read(&same_tool), allow());
        assert_eq!(read(&other_tool), None);
        assert_eq!(read(&other_session), None);
        let audits = read(&f.audits);
        assert_eq!(
            audits.iter().map(|a| a.auto).collect::<Vec<_>>(),
            [false, true]
        );
        // The untouched requests keep their order.
        assert_eq!(
            f.store
                .permission_queue()
                .iter()
                .map(|entry| entry.request()["toolCall"]["title"].clone())
                .collect::<Vec<_>>(),
            [
                json!("mcp__rever-traffic__intruder_run"),
                json!("mcp__rever-traffic__burst_send")
            ]
        );

        // A later call of the granted tool is allowed without a prompt, and audited.
        let (id, later) = f.request(risky_req("s1"));
        assert_eq!(id, None);
        assert_eq!(read(&later), allow());
        assert_eq!(read(&f.audits).len(), 3);

        f.store.session_grants_mut().clear_session_grants();
        let (id, _) = f.request(risky_req("s1"));
        assert!(id.is_some());
    }

    #[test]
    fn 거절한_응답은_세션_허용을_남기지_않는다() {
        let mut f = Fixture::new();
        f.request(risky_req("s1"));
        f.store.respond_to_permission(
            "reject",
            ApproveOptions {
                grant_session: true,
            },
        );
        assert!(
            !f.store
                .session_grants()
                .has_session_grant(&risky_req("s1"), &TsRiskyTools)
        );

        f.request(risky_req("s1"));
        f.store.respond_to_permission(
            "allow",
            ApproveOptions {
                grant_session: true,
            },
        );
        assert!(
            f.store
                .session_grants()
                .has_session_grant(&risky_req("s1"), &TsRiskyTools)
        );
    }

    #[test]
    fn 취소는_거절로_감사_기록을_남기고_응답_json은_acp_모양이다() {
        let mut f = Fixture::new();
        f.request(risky_req("s1"));
        f.store.cancel_pending_permissions("s1");
        assert_eq!(
            read(&f.audits),
            [ApprovalAudit {
                tool: "burst_send".to_string(),
                target: None,
                decision: Decision::Reject,
                auto: true,
                risky: true,
            }]
        );
        assert_eq!(
            serde_json::to_value(read(&f.audits)).unwrap_or_default(),
            json!([{ "tool": "burst_send", "target": null, "decision": "reject", "auto": true, "risky": true }])
        );
        // A request without options or a tool call must not panic.
        f.store.set_acp_auto_approve(true);
        let (id, r) = f.request(json!({ "sessionId": "s9" }));
        assert_eq!(id, None);
        assert_eq!(read(&r), selected_json(""));
    }
}
