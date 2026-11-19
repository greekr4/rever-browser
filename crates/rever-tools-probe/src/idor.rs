//! Port of `src/main/mcp/tools/idor_probe.ts` — three tools for broken
//! authorization, replayed over a RAW socket because browser fetch cannot
//! override the `Cookie` header (forbidden) nor drop httpOnly cookies
//! (`credentials:include` re-attaches them), which these auth tests must control:
//! - `noauth_probe`: broken authentication (OWASP API2).
//! - `redteam_alt_session_set`: store a second tenant's cookie for `idor_probe`.
//! - `idor_probe`: BOLA / IDOR (broken object-level authorization).
//!
//! Gated to loopback / lab hosts, like `raw_http`.

use std::time::Duration;

use rever_core::traffic::Headers;
use rever_scope::fence::{is_lab_host, is_loopback_host};
use rever_scope::redteam_tenants::{get_alt_session, set_alt_session};
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, err, ok};
use rever_tools_http::{
    RequestSpec, build_raw_request, build_request_spec, parse_raw_response, restore_marker,
    send_raw_over_socket,
};
use serde_json::{Value, json};
use url::Url;

use crate::args::{text, validate};
use crate::{pretty, schema};

const RAW_TIMEOUT: Duration = Duration::from_secs(8);

/// A raw-socket replay result.
struct RawReply {
    label: &'static str,
    status: u16,
    body: String,
    bytes: usize,
}

/// Copy request headers, drop the ones `build_raw_request` manages + any Cookie,
/// then set Cookie to `cookie` (or leave it absent when `None`).
fn with_cookie(headers: &Headers, cookie: Option<&str>) -> Vec<(String, String)> {
    const DROP: [&str; 4] = ["cookie", "host", "content-length", "connection"];
    let mut out: Vec<(String, String)> = headers
        .iter()
        .filter(|(name, _)| !DROP.contains(&name.to_ascii_lowercase().as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    if let Some(cookie) = cookie {
        out.push(("Cookie".to_owned(), cookie.to_owned()));
    }
    out
}

/// The Cookie header value already on the captured request, if any.
fn original_cookie(headers: &Headers) -> Option<String> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("cookie"))
        .map(|(_, value)| value.clone())
}

/// A whitespace-stripped head of the body — enough to tell two JSON resources
/// apart without being fooled by formatting.
fn fingerprint(b: &str) -> String {
    b.chars().filter(|c| !c.is_whitespace()).take(400).collect()
}

/// Whether `b` returned the same distinctive resource as `a`.
fn same_resource(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let fa = fingerprint(a);
    if fa.chars().count() < 20 {
        return false; // too small to be a distinctive resource
    }
    let fb: String = b.chars().filter(|c| !c.is_whitespace()).collect();
    if fingerprint(b) == fa {
        return true;
    }
    let head: String = fa.chars().take(64).collect();
    fb.contains(&head)
}

/// Same cookie names, garbage values — the negative control for broken auth.
fn bogus_cookie(owner: &str) -> String {
    owner
        .split(';')
        .map(|p| match p.find('=') {
            None => p.to_owned(),
            Some(i) => format!("{}=reverbogus0", p[..i].trim()),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn ok_2xx(status: u16) -> bool {
    (200..300).contains(&status)
}

/// BOLA decision: confirmed when the cross-tenant session reads the same
/// resource anonymous could not.
fn idor_decision(baseline: &RawReply, noauth: &RawReply, cross: &RawReply) -> (bool, String) {
    if !ok_2xx(baseline.status) {
        return (false, format!("baseline {}: the resource isn't readable even as its owner — can't assess BOLA (bad requestId or expired owner cookie).", baseline.status));
    }
    if ok_2xx(noauth.status) && same_resource(&baseline.body, &noauth.body) {
        return (false, format!("no-auth replay returned the same resource ({}): it is readable with NO session at all — that's a public-exposure / broken-auth leak, not object-level auth. Record as info-disclosure.", noauth.status));
    }
    if ok_2xx(cross.status) && same_resource(&baseline.body, &cross.body) {
        return (true, format!("the alternate tenant's session read the owner's resource (HTTP {}, same body) while anonymous was denied ({}) — the server authorizes on the object id alone. BOLA/IDOR confirmed (cross-tenant data access).", cross.status, noauth.status));
    }
    let suffix = if ok_2xx(cross.status) {
        " with a different body"
    } else {
        ""
    };
    (false, format!("cross-tenant session got {}{suffix}: the resource appears scoped to its owner.", cross.status))
}

/// Broken-auth decision: confirmed when a cookieless request returns the same
/// protected resource AND a garbage cookie is denied.
fn noauth_decision(baseline: &RawReply, noauth: &RawReply, bogus: &RawReply) -> (bool, String) {
    if !ok_2xx(baseline.status) {
        return (false, format!("baseline {}: the resource isn't readable even with the owner session — can't assess (bad requestId or no owner session in the partition).", baseline.status));
    }
    if !(ok_2xx(noauth.status) && same_resource(&baseline.body, &noauth.body)) {
        let suffix = if ok_2xx(noauth.status) {
            " with a different/empty body"
        } else {
            ""
        };
        return (false, format!("no-cookie request got {}{suffix}: authentication is enforced (not broken auth).", noauth.status));
    }
    if ok_2xx(bogus.status) && same_resource(&baseline.body, &bogus.body) {
        return (false, format!("served identically with NO cookie AND a garbage cookie (HTTP {}/{}) — the endpoint ignores identity, consistent with PUBLIC-BY-DESIGN, not proven broken auth. Record as candidate (category:endpoint/info) and confirm whether it is meant to be public; a genuine broken-auth leak returns owner-specific data a wrong identity is denied.", noauth.status, bogus.status));
    }
    (true, format!("the resource is returned with NO cookie (HTTP {}, same body as the owner) while a GARBAGE cookie is rejected ({}) — the server enforces sessions on this path but the no-cookie request bypasses it. Broken auth (API2) confirmed over a raw socket.", noauth.status, bogus.status))
}

/// Rebuild the captured request over a raw socket with `cookie` (or none).
fn raw_replay(base: &RequestSpec, u: &Url, cookie: Option<&str>, label: &'static str) -> RawReply {
    let host = u.host_str().unwrap_or("");
    let port = u
        .port_or_known_default()
        .unwrap_or(if u.scheme() == "https" { 443 } else { 80 });
    let tls = u.scheme() == "https";
    let path_and_search = match u.query() {
        Some(q) => format!("{}?{q}", u.path()),
        None => u.path().to_owned(),
    };
    let headers = with_cookie(&base.headers, cookie);
    let payload = build_raw_request(
        &base.method,
        &path_and_search,
        &headers,
        base.body.as_deref(),
        &format!("{host}:{port}"),
    );
    // Latin-1 encode, as raw_http does: each char 0..=255 is one byte.
    let bytes: Vec<u8> = payload.chars().map(|c| c as u8).collect();
    match send_raw_over_socket(host, port, &bytes, tls, RAW_TIMEOUT) {
        Ok(raw) => {
            let parsed = parse_raw_response(&raw);
            RawReply {
                label,
                status: parsed.status,
                bytes: parsed.body.chars().count(),
                body: parsed.body,
            }
        }
        Err(_) => RawReply {
            label,
            status: 0,
            body: String::new(),
            bytes: 0,
        },
    }
}

/// Build the base spec from the store and parse its URL, or return a tool error.
/// On success also checks the loopback / lab gate.
async fn base_and_url(
    ctx: &ToolContext,
    request_id: &str,
    tool: &str,
) -> Result<(RequestSpec, Url), ToolResult> {
    let base = {
        let store = ctx
            .traffic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match build_request_spec(&store, Some(request_id), None) {
            Ok(spec) => restore_marker(spec),
            Err(error) => return Err(err(error)),
        }
    };
    let Ok(u) = Url::parse(&base.url) else {
        return Err(err(format!("captured request has no valid URL: {}", base.url)));
    };
    if !is_loopback_host(&base.url) && !is_lab_host(&base.url) {
        let host = u.host_str().unwrap_or("");
        return Err(err(format!(
            "{tool} sends raw sockets, gated to loopback / lab hosts; {host} is neither. Mark it a lab target if you own it."
        )));
    }
    Ok((base, u))
}

/// The owner's live session cookies from the logged-in partition (httpOnly
/// included) — captured XHRs often omit the Cookie header.
async fn partition_cookie_header(ctx: &ToolContext, url: &str) -> Option<String> {
    let cookies = ctx.browser.cookies(url).await.ok()?;
    if cookies.is_empty() {
        return None;
    }
    Some(
        cookies
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn results_json(replies: &[&RawReply]) -> Value {
    json!(
        replies
            .iter()
            .map(|r| json!({ "label": r.label, "status": r.status, "bytes": r.bytes }))
            .collect::<Vec<_>>()
    )
}

pub struct NoauthProbe;

impl Tool for NoauthProbe {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "noauth_probe",
            "Confirm broken authentication (OWASP API2) reliably — use this instead of replay_request-with-cookie-blanked, which CANNOT drop httpOnly session cookies (browser fetch re-attaches them via credentials:include) and so reports false \"no-auth 200\"s. Replays the captured request three ways over a raw socket: owner session (baseline, auto-read from the logged-in partition), NO cookie, and a GARBAGE cookie (negative control). Broken auth is confirmed only when the cookieless request returns the SAME protected resource AND the garbage cookie is denied — if the garbage cookie is served the same resource, the endpoint ignores identity (public-by-design) and is NOT confirmed. Gated to loopback / lab hosts.",
            schema(
                json!({
                    "requestId": { "type": "string", "description": "A captured authenticated request to a protected resource" },
                    "ownerCookie": { "type": "string", "description": "Owner Cookie value (defaults to the logged-in partition session)" }
                }),
                &["requestId"],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            let request_id = text(&args, "requestId").unwrap_or_default().to_owned();
            let (base, u) = match base_and_url(ctx, &request_id, "noauth_probe").await {
                Ok(pair) => pair,
                Err(refusal) => return refusal,
            };
            let owner = match owner_cookie(ctx, &args, &base).await {
                Some(owner) => owner,
                None => {
                    return err("no owner session available (capture had no Cookie and the partition has none for this origin) — pass ownerCookie.");
                }
            };
            let baseline = raw_replay(&base, &u, Some(&owner), "baseline");
            let noauth = raw_replay(&base, &u, None, "noauth");
            let bogus = raw_replay(&base, &u, Some(&bogus_cookie(&owner)), "bogus");
            let (broken_auth, note) = noauth_decision(&baseline, &noauth, &bogus);
            ok(pretty(&json!({
                "brokenAuth": broken_auth,
                "note": note,
                "results": results_json(&[&baseline, &noauth, &bogus]),
            })))
        })
    }
}

pub struct RedteamAltSessionSet;

impl Tool for RedteamAltSessionSet {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "redteam_alt_session_set",
            "Store a SECOND tenant's session Cookie for idor_probe to use as the cross-tenant attacker. Call this during auth bootstrap after registering + logging in a throwaway account: read its Cookie (cookie_list), then pass the full `Cookie:` header value here. idor_probe then replays owner resources with this cookie to confirm cross-tenant access.",
            schema(
                json!({
                    "cookie": { "type": "string", "description": "The full Cookie header value of the alternate (attacker) tenant session" }
                }),
                &["cookie"],
            ),
        )
    }

    fn call<'a>(&'a self, _ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            let cookie = text(&args, "cookie").unwrap_or_default();
            set_alt_session(Some(cookie));
            ok("alternate-tenant session stored — idor_probe will use it as the cross-tenant attacker.")
        })
    }
}

pub struct IdorProbe;

impl Tool for IdorProbe {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "idor_probe",
            "Confirm BOLA / IDOR (broken object-level authorization) at an authenticated resource — the active test for UUID/id-keyed cookie-auth APIs where injection probes do not apply. Replays the captured OWNER request three ways over a raw socket: owner cookie (baseline), no cookie (noauth), and the alternate tenant cookie set via redteam_alt_session_set (cross). Confirms BOLA when the cross-tenant session reads the same resource that anonymous could not. Needs an alternate-tenant session (set it first) and a captured authenticated request. Gated to loopback / lab hosts.",
            schema(
                json!({
                    "requestId": { "type": "string", "description": "A captured authenticated request to a UUID/id-keyed resource (the owner baseline)" },
                    "altCookie": { "type": "string", "description": "Alternate-tenant Cookie value (defaults to the one set via redteam_alt_session_set)" },
                    "ownerCookie": { "type": "string", "description": "Owner Cookie value, if the captured request did not carry one" }
                }),
                &["requestId"],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            let request_id = text(&args, "requestId").unwrap_or_default().to_owned();
            let (base, u) = match base_and_url(ctx, &request_id, "idor_probe").await {
                Ok(pair) => pair,
                Err(refusal) => return refusal,
            };
            let alt = match text(&args, "altCookie").map(str::to_owned).or_else(get_alt_session) {
                Some(alt) => alt,
                None => {
                    return err("no alternate-tenant session: register a 2nd throwaway account, read its cookie (cookie_list), then redteam_alt_session_set({ cookie }) — or pass altCookie here.");
                }
            };
            let owner = match owner_cookie(ctx, &args, &base).await {
                Some(owner) => owner,
                None => {
                    return err("no owner session for the baseline (capture had no Cookie and the partition has none for this origin) — pass ownerCookie.");
                }
            };
            let baseline = raw_replay(&base, &u, Some(&owner), "baseline");
            let noauth = raw_replay(&base, &u, None, "noauth");
            let cross = raw_replay(&base, &u, Some(&alt), "cross");
            let (bola, note) = idor_decision(&baseline, &noauth, &cross);
            ok(pretty(&json!({
                "bola": bola,
                "note": note,
                "results": results_json(&[&baseline, &noauth, &cross]),
            })))
        })
    }
}

/// Owner cookie: the `ownerCookie` arg, else the captured request's Cookie, else
/// the logged-in partition's cookies for the origin.
async fn owner_cookie(ctx: &ToolContext, args: &Value, base: &RequestSpec) -> Option<String> {
    if let Some(cookie) = text(args, "ownerCookie") {
        return Some(cookie.to_owned());
    }
    if let Some(cookie) = original_cookie(&base.headers) {
        return Some(cookie);
    }
    partition_cookie_header(ctx, &base.url).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Headers {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn reply(label: &'static str, status: u16, body: &str) -> RawReply {
        RawReply {
            label,
            status,
            body: body.to_owned(),
            bytes: body.len(),
        }
    }

    #[test]
    fn with_cookie는_관리헤더와_기존쿠키를_버리고_새쿠키를_넣는다() {
        let h = headers(&[
            ("Accept", "application/json"),
            ("Cookie", "old=1"),
            ("Host", "x"),
            ("Content-Length", "5"),
            ("Connection", "keep-alive"),
        ]);
        assert_eq!(
            with_cookie(&h, Some("ats_token=B")),
            vec![
                ("Accept".to_owned(), "application/json".to_owned()),
                ("Cookie".to_owned(), "ats_token=B".to_owned()),
            ]
        );
    }

    #[test]
    fn with_cookie_none이면_쿠키를_넣지_않는다() {
        assert_eq!(
            with_cookie(&headers(&[("Accept", "json"), ("cookie", "old=1")]), None),
            vec![("Accept".to_owned(), "json".to_owned())]
        );
        assert_eq!(
            with_cookie(&headers(&[("COOKIE", "old=1")]), Some("new=2")),
            vec![("Cookie".to_owned(), "new=2".to_owned())]
        );
    }

    #[test]
    fn original_cookie는_대소문자_무관하게_뽑는다() {
        assert_eq!(
            original_cookie(&headers(&[("cookie", "ats_token=A")])).as_deref(),
            Some("ats_token=A")
        );
        assert_eq!(original_cookie(&headers(&[("Accept", "json")])), None);
    }

    #[test]
    fn same_resource는_공백차이는_무시하고_짧은본문은_판정안한다() {
        assert!(same_resource(
            "{\"id\":\"abc\",\"name\":\"X\"}",
            "{ \"id\": \"abc\", \"name\": \"X\" }"
        ));
        let owner = "{\"id\":\"cdb11ac2-9993-4ac6-bb9d-6959df5228a9\",\"applicants\":[{\"email\":\"a@b.com\"}]}";
        assert!(same_resource(owner, &format!("prefix...{owner}...suffix")));
        assert!(!same_resource(
            "{\"id\":\"abc\",\"data\":1}",
            "{\"error\":\"forbidden\"}"
        ));
        assert!(!same_resource("{}", "{}"));
    }

    #[test]
    fn bogus_cookie는_이름은_유지하고_값만_바꾼다() {
        assert_eq!(
            bogus_cookie("session=abc123; ats_token=eyJhbGc.iJ"),
            "session=reverbogus0; ats_token=reverbogus0"
        );
    }

    const OWNER: &str =
        "{\"id\":\"cdb11ac2-9993-4ac6-bb9d-6959df5228a9\",\"applicants\":[{\"email\":\"uoojin12@naver.com\"}]}";

    #[test]
    fn idor_익명거부_타테넌트_동일리소스면_확정() {
        let (bola, _) = idor_decision(
            &reply("baseline", 200, OWNER),
            &reply("noauth", 401, "{\"message\":\"Unauthorized\"}"),
            &reply("cross", 200, OWNER),
        );
        assert!(bola);
    }

    #[test]
    fn idor_익명도_같은리소스면_공개노출_음성() {
        let (bola, note) = idor_decision(
            &reply("baseline", 200, OWNER),
            &reply("noauth", 200, OWNER),
            &reply("cross", 200, OWNER),
        );
        assert!(!bola);
        let lower = note.to_lowercase();
        assert!(
            lower.contains("public-exposure")
                || lower.contains("broken-auth")
                || lower.contains("info-disclosure")
        );
    }

    #[test]
    fn idor_타테넌트_403이나_다른본문이면_음성() {
        let (bola, _) = idor_decision(
            &reply("baseline", 200, OWNER),
            &reply("noauth", 401, "no"),
            &reply("cross", 403, "{\"message\":\"Forbidden\"}"),
        );
        assert!(!bola);
        let (bola, _) = idor_decision(
            &reply("baseline", 200, OWNER),
            &reply("noauth", 401, "no"),
            &reply("cross", 200, "{\"id\":\"other\",\"applicants\":[]}"),
        );
        assert!(!bola);
    }

    #[test]
    fn idor_baseline이_2xx가_아니면_판정불가() {
        let (bola, note) = idor_decision(
            &reply("baseline", 404, "not found"),
            &reply("noauth", 401, "no"),
            &reply("cross", 200, OWNER),
        );
        assert!(!bola);
        assert!(note.to_lowercase().contains("baseline"));
    }

    const AUTH_OWNER: &str =
        "{\"id\":\"cdb11ac2-9993-4ac6-bb9d-6959df5228a9\",\"member\":{\"email\":\"te@test.com\"}}";

    #[test]
    fn noauth_무쿠키_보호리소스_쓰레기쿠키거부면_확정() {
        let (broken, _) = noauth_decision(
            &reply("baseline", 200, AUTH_OWNER),
            &reply("noauth", 200, AUTH_OWNER),
            &reply("bogus", 401, "{\"message\":\"Unauthorized\"}"),
        );
        assert!(broken);
    }

    #[test]
    fn noauth_무쿠키_쓰레기쿠키_모두_같으면_공개설계_음성() {
        let (broken, note) = noauth_decision(
            &reply("baseline", 200, AUTH_OWNER),
            &reply("noauth", 200, AUTH_OWNER),
            &reply("bogus", 200, AUTH_OWNER),
        );
        assert!(!broken);
        assert!(note.to_lowercase().contains("public-by-design"));
    }

    #[test]
    fn noauth_쿠키없으면_401이면_정상보호_음성() {
        let (broken, _) = noauth_decision(
            &reply("baseline", 200, AUTH_OWNER),
            &reply("noauth", 401, "{\"message\":\"Unauthorized\"}"),
            &reply("bogus", 401, "no"),
        );
        assert!(!broken);
    }

    #[test]
    fn noauth_무쿠키_200이나_다른본문이면_음성() {
        let (broken, _) = noauth_decision(
            &reply("baseline", 200, AUTH_OWNER),
            &reply("noauth", 200, "{\"session\":null}"),
            &reply("bogus", 200, "{\"session\":null}"),
        );
        assert!(!broken);
    }

    #[test]
    fn noauth_baseline이_2xx가_아니면_판정불가() {
        let (broken, note) = noauth_decision(
            &reply("baseline", 500, "err"),
            &reply("noauth", 401, "no"),
            &reply("bogus", 401, "no"),
        );
        assert!(!broken);
        assert!(note.to_lowercase().contains("baseline"));
    }
}
