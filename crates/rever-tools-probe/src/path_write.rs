//! Port of `src/main/mcp/tools/path_write_probe.ts` — arbitrary-file-write /
//! path-traversal probe for a "stored then displayed" filename sink. Write a
//! unique marker twice (plain filename, then `../` filename) and diff the
//! readback page: marker seen for the plain name but NOT the traversal name =
//! the `../` left the served directory = path traversal in the write path.

use std::sync::PoisonError;
use std::time::Duration;

use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, err, ok};
use rever_tools_http::{RequestSpec, build_request_spec, restore_marker, send};
use serde_json::{Value, json};
use url::{Url, form_urlencoded};

use crate::args::{text, validate};
use crate::common::random_nonce;
use crate::{pretty, schema};

const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// Marker contents and filename values for the two writes.
pub struct PathWritePlan {
    /// Unique marker written as the file CONTENT and searched for on readback.
    pub marker1: String,
    pub marker2: String,
    /// Filename value for the tested param: plain, then traversal (one level up).
    pub fname1: String,
    pub fname2: String,
}

/// The caller supplies the nonce (default `crate::common::random_nonce()`).
pub fn build_path_write_plan(nonce: &str) -> PathWritePlan {
    PathWritePlan {
        marker1: format!("rpw1{nonce}"),
        marker2: format!("rpw2{nonce}"),
        fname1: format!("rpwf1{nonce}"),
        fname2: format!("../rpwf2{nonce}"),
    }
}

/// The verdict of the differential readback.
pub struct PathWriteDecision {
    pub confirmed: bool,
    pub kind: &'static str,
}

/// plain seen but traversal NOT → the `../` escaped the served dir = traversal.
/// Both seen → stored but not moved. Plain not seen → no readback hook.
pub fn path_write_decision(plain_seen: bool, trav_seen: bool) -> PathWriteDecision {
    match (plain_seen, trav_seen) {
        (true, false) => PathWriteDecision {
            confirmed: true,
            kind: "traversal",
        },
        (true, true) => PathWriteDecision {
            confirmed: false,
            kind: "stored-no-traversal",
        },
        _ => PathWriteDecision {
            confirmed: false,
            kind: "inconclusive",
        },
    }
}

/// Set the tested param to `value` and every OTHER param to `marker`, for both
/// query string and an x-www-form-urlencoded body. Returns the rewritten pair.
pub fn place_markers(
    url: &str,
    body: Option<&str>,
    param: &str,
    value: &str,
    marker: &str,
) -> (String, Option<String>) {
    // keys in order, deduped — mirrors `[...sp.keys()]` + `set` collapsing dups.
    let ordered_keys = |pairs: form_urlencoded::Parse| -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for (k, _) in pairs {
            let k = k.into_owned();
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        keys
    };
    let fill = |keys: &[String]| -> String {
        let mut ser = form_urlencoded::Serializer::new(String::new());
        for k in keys {
            ser.append_pair(k, if k == param { value } else { marker });
        }
        ser.finish()
    };
    let rewrite_query = |u: &str| -> String {
        let Ok(mut parsed) = Url::parse(u) else {
            return u.to_owned();
        };
        let keys = ordered_keys(parsed.query_pairs());
        let mut serialized = fill(&keys);
        if !keys.iter().any(|k| k == param) {
            // append param=value when it was in neither spot
            let mut ser = form_urlencoded::Serializer::new(serialized);
            ser.append_pair(param, value);
            serialized = ser.finish();
        }
        parsed.set_query(Some(&serialized));
        parsed.to_string()
    };
    let rewrite_body =
        |b: &str| -> String { fill(&ordered_keys(form_urlencoded::parse(b.as_bytes()))) };

    let url_has_param = Url::parse(url)
        .map(|u| u.query_pairs().any(|(k, _)| k == param))
        .unwrap_or(false);
    // Put the tested param where it lives; if it's in neither, add it to the
    // query (the common stored-form case is a GET like Testimonial).
    let new_url = if url_has_param || body.is_none() {
        rewrite_query(url)
    } else {
        url.to_owned()
    };
    (new_url, body.map(rewrite_body))
}

pub struct PathWriteProbe;

impl Tool for PathWriteProbe {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "path_write_probe",
            "Confirm arbitrary file write / path traversal at a param whose value becomes a server-side FILENAME (stored-then-displayed sinks, e.g. a testimonial/comment/upload name). Writes a unique marker as the content twice — once with a plain filename, once with a `../` filename — then re-fetches the readback page: if the marker appears for the plain name but NOT for the traversal name, the `../` left the served directory, proving path traversal in the write path. Use this when a reflection probe says \"stored but escaped (no XSS/SSTI)\" on a multi-field form: the filename field is the real sink. A positive result is arbitrary-file-write class (→ overwrite source/templates = RCE); record as high/critical.",
            schema(
                json!({
                    "requestId": { "type": "string", "description": "The captured write request (the form submit that stores content)" },
                    "param": { "type": "string", "description": "The param carrying the FILENAME (e.g. \"customer\"); other params are filled with the marker content" },
                    "readback": { "type": "string", "description": "Absolute URL that displays stored content (default: the request URL without its query, i.e. the app root)" }
                }),
                &["requestId", "param"],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            let request_id = text(&args, "requestId").unwrap_or_default().to_owned();
            let param = text(&args, "param").unwrap_or_default().to_owned();
            let readback_arg = text(&args, "readback").map(str::to_owned);

            // Build the base spec under the lock, then drop it before any await.
            let base = {
                let store = ctx.traffic.lock().unwrap_or_else(PoisonError::into_inner);
                match build_request_spec(&store, Some(&request_id), None) {
                    Ok(spec) => restore_marker(spec),
                    Err(error) => return err(error),
                }
            };

            let readback_url = readback_arg.unwrap_or_else(|| match Url::parse(&base.url) {
                Ok(u) => format!("{}{}", u.origin().ascii_serialization(), u.path()),
                Err(_) => base.url.clone(),
            });

            let plan = build_path_write_plan(&random_nonce());
            let fetch_readback = || async {
                let out = send(
                    &RequestSpec {
                        url: readback_url.clone(),
                        method: "GET".to_owned(),
                        headers: Default::default(),
                        body: None,
                    },
                    SEND_TIMEOUT,
                )
                .await;
                out.body
            };

            // 1) plain filename + marker1 content
            let (w1_url, w1_body) = place_markers(
                &base.url,
                base.body.as_deref(),
                &param,
                &plan.fname1,
                &plan.marker1,
            );
            send(
                &RequestSpec {
                    url: w1_url,
                    method: base.method.clone(),
                    headers: base.headers.clone(),
                    body: w1_body,
                },
                SEND_TIMEOUT,
            )
            .await;
            let plain_seen = fetch_readback().await.contains(&plan.marker1);

            // 2) traversal filename + marker2 content
            let (w2_url, w2_body) = place_markers(
                &base.url,
                base.body.as_deref(),
                &param,
                &plan.fname2,
                &plan.marker2,
            );
            send(
                &RequestSpec {
                    url: w2_url,
                    method: base.method.clone(),
                    headers: base.headers.clone(),
                    body: w2_body,
                },
                SEND_TIMEOUT,
            )
            .await;
            let trav_seen = fetch_readback().await.contains(&plan.marker2);

            let decision = path_write_decision(plain_seen, trav_seen);
            let note = match decision.kind {
                "traversal" => {
                    "Arbitrary file write via path traversal CONFIRMED: the plain-name marker rendered on readback but the `../` marker did not, so the filename param escapes the intended directory. Writing a source/template file (e.g. .templ/.html/.php) = RCE. Record high/critical."
                }
                "stored-no-traversal" => {
                    "Content is stored and displayed, but `../` did not change where it landed (sanitised or single dir) — stored content injection, not traversal."
                }
                _ => {
                    "Could not see the marker on the readback page — wrong readback URL, or the value is not displayed. Pass readback=<url that shows stored content> and retry."
                }
            };
            ok(pretty(&json!({
                "arbitraryWrite": decision.confirmed,
                "kind": decision.kind,
                "plainSeen": plain_seen,
                "travSeen": trav_seen,
                "readback": readback_url,
                "param": param,
                "note": note,
            })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_get(url: &str, key: &str) -> Option<String> {
        Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    }

    fn body_get(body: &str, key: &str) -> Option<String> {
        form_urlencoded::parse(body.as_bytes())
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    }

    #[test]
    fn 평문_트래버설_파일명과_서로_다른_마커를_만든다() {
        let p = build_path_write_plan("zABCz");
        assert_eq!(p.fname1, "rpwf1zABCz");
        assert_eq!(p.fname2, "../rpwf2zABCz");
        assert_ne!(p.marker1, p.marker2);
        // 트래버설만 ../ 를 가진다
        assert!(!p.fname1.contains(".."));
        assert!(p.fname2.starts_with("../"));
    }

    #[test]
    fn 평문_보이고_트래버설_안_보이면_traversal_확정() {
        let d = path_write_decision(true, false);
        assert!(d.confirmed);
        assert_eq!(d.kind, "traversal");
    }

    #[test]
    fn 둘_다_보이면_저장은_되나_traversal_미확정() {
        let d = path_write_decision(true, true);
        assert!(!d.confirmed);
        assert_eq!(d.kind, "stored-no-traversal");
    }

    #[test]
    fn 평문도_안_보이면_inconclusive_되읽기_훅_없음() {
        let d = path_write_decision(false, false);
        assert!(!d.confirmed);
        assert_eq!(d.kind, "inconclusive");
        let d = path_write_decision(false, true);
        assert!(!d.confirmed);
        assert_eq!(d.kind, "inconclusive");
    }

    #[test]
    fn 테스트_파라미터는_파일명_나머지_쿼리는_마커로_채운다_testimonial_get() {
        let (url, _) = place_markers(
            "http://localhost:1337/?customer=&testimonial=",
            None,
            "customer",
            "../evil",
            "MARK",
        );
        assert_eq!(query_get(&url, "customer").as_deref(), Some("../evil"));
        assert_eq!(query_get(&url, "testimonial").as_deref(), Some("MARK"));
    }

    #[test]
    fn 쿼리에_없던_테스트_파라미터는_추가한다() {
        let (url, _) = place_markers("http://x/?a=1", None, "customer", "fn", "MARK");
        assert_eq!(query_get(&url, "a").as_deref(), Some("MARK"));
        assert_eq!(query_get(&url, "customer").as_deref(), Some("fn"));
    }

    #[test]
    fn 폼_바디의_다른_필드도_마커로_채운다() {
        let (_, body) = place_markers("http://x/", Some("name=&content="), "name", "../fn", "MARK");
        let body = body.unwrap();
        assert_eq!(body_get(&body, "name").as_deref(), Some("../fn"));
        assert_eq!(body_get(&body, "content").as_deref(), Some("MARK"));
    }
}
