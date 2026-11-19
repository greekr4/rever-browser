//! Port of `src/main/mcp/request-diff.ts` — pure request-comparison helpers.
//!
//! The records the TypeScript diffs are JavaScript objects; here they are
//! `serde_json::Map`s kept in the order JavaScript would enumerate them, so
//! the changes come out in the same order.

use std::collections::HashMap;

use rever_core::traffic::{Headers, StoredRequest};
use serde::Serialize;
use serde_json::{Map, Value};
use url::Url;

use crate::{js_key_order, json_parse};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeType {
    Added,
    Removed,
    Changed,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Change {
    #[serde(rename = "type")]
    pub kind: ChangeType,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Value>,
}

/// Query parameters by name; a repeated name keeps its last value.
pub fn parse_query(url: &str) -> Map<String, Value> {
    let Ok(u) = Url::parse(url) else {
        return Map::new();
    };
    let mut obj = Map::new();
    for (k, v) in u.query_pairs() {
        obj.insert(k.into_owned(), Value::String(v.into_owned()));
    }
    js_key_order(obj)
}

pub fn diff_objects(label: &str, a: &Map<String, Value>, b: &Map<String, Value>) -> Vec<Change> {
    let mut changes = Vec::new();
    let all_keys = a.keys().chain(b.keys().filter(|k| !a.contains_key(*k)));
    for k in all_keys {
        let key = format!("{label}.{k}");
        let change = |kind, from: Option<&Value>, to: Option<&Value>| Change {
            kind,
            key,
            from: from.cloned(),
            to: to.cloned(),
        };
        match (a.get(k), b.get(k)) {
            (None, to) => changes.push(change(ChangeType::Added, None, to)),
            (from, None) => changes.push(change(ChangeType::Removed, from, None)),
            (Some(from), Some(to)) => {
                // Compared as JSON text like the TypeScript, so key order inside
                // a nested object counts as a change; `Value` equality ignores it.
                let (from_text, to_text) = (from.to_string(), to.to_string());
                if from_text != to_text {
                    changes.push(change(ChangeType::Changed, Some(from), Some(to)));
                }
            }
        }
    }
    changes
}

/// A JSON object, or an array as the index-keyed object JavaScript sees it as.
pub fn try_parse_json_object(s: Option<&str>) -> Option<Map<String, Value>> {
    match json_parse(s.filter(|s| !s.is_empty())?)? {
        Value::Object(map) => Some(map),
        Value::Array(items) => Some(
            items
                .into_iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v))
                .collect(),
        ),
        _ => None,
    }
}

fn lower(h: Option<&Headers>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in h.into_iter().flatten() {
        out.insert(k.to_lowercase(), Value::String(v.clone()));
    }
    js_key_order(out)
}

/// All query/header/body changes between two requests, in one flat list.
pub fn diff_requests(a: &StoredRequest, b: &StoredRequest) -> Vec<Change> {
    let mut changes = diff_objects("query", &parse_query(&a.url), &parse_query(&b.url));

    changes.extend(diff_objects(
        "headers",
        &lower(a.request_headers.as_ref()),
        &lower(b.request_headers.as_ref()),
    ));

    let body_a = try_parse_json_object(a.request_post_data.as_deref());
    let body_b = try_parse_json_object(b.request_post_data.as_deref());
    if let (Some(body_a), Some(body_b)) = (&body_a, &body_b) {
        changes.extend(diff_objects("body", body_a, body_b));
    } else if a.request_post_data != b.request_post_data {
        changes.push(Change {
            kind: ChangeType::Changed,
            key: "body".into(),
            from: a.request_post_data.clone().map(Value::String),
            to: b.request_post_data.clone().map(Value::String),
        });
    }
    changes
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ApiBase {
    pub base: String,
    pub count: usize,
}

/// The TypeScript's default for `top`.
pub const API_BASE_TOP: usize = 5;

/// Most common host + first-two-path-segments across requests.
pub fn compute_api_base<S: AsRef<str>>(urls: &[S], top: usize) -> Vec<ApiBase> {
    let mut bases: Vec<ApiBase> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for url in urls {
        // skip unparseable
        let Ok(u) = Url::parse(url.as_ref()) else {
            continue;
        };
        let prefix: Vec<&str> = u
            .path()
            .split('/')
            .filter(|s| !s.is_empty())
            .take(2)
            .collect();
        // `u.host` in JavaScript: the hostname, plus the port unless it is the scheme's default.
        let host = u.host_str().unwrap_or("");
        let port = u.port().map_or(String::new(), |p| format!(":{p}"));
        let key = format!("{}://{host}{port}/{}", u.scheme(), prefix.join("/"));
        let at = *by_key.entry(key.clone()).or_insert(bases.len());
        match bases.get_mut(at) {
            Some(base) => base.count += 1,
            None => bases.push(ApiBase {
                base: key,
                count: 1,
            }),
        }
    }
    // Stable, like Array.prototype.sort: ties keep first-seen order.
    bases.sort_by_key(|b| std::cmp::Reverse(b.count));
    bases.truncate(top);
    bases
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn obj(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    fn summary(changes: &[Change]) -> Vec<String> {
        changes
            .iter()
            .map(|c| serde_json::to_string(c).unwrap())
            .collect()
    }

    fn request(url: &str, headers: &[(&str, &str)], body: Option<&str>) -> StoredRequest {
        StoredRequest {
            url: url.into(),
            request_headers: Some(
                headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            request_post_data: body.map(str::to_owned),
            ..StoredRequest::default()
        }
    }

    // ── cases of src/main/mcp/pure-helpers.test.ts ──────────────────────────

    #[test]
    fn 쿼리_문자열을_파싱하고_해석할_수_없으면_빈_객체를_돌려준다() {
        assert_eq!(
            parse_query("https://a.com/x?a=1&b=2"),
            obj(json!({"a": "1", "b": "2"}))
        );
        assert_eq!(parse_query("not a url"), Map::new());
    }

    #[test]
    fn 추가_삭제_변경된_키를_분류한다() {
        let changes = diff_objects(
            "q",
            &obj(json!({"a": 1, "b": 2})),
            &obj(json!({"b": 3, "c": 4})),
        );
        let change = |kind, key: &str, from: Option<i32>, to: Option<i32>| Change {
            kind,
            key: key.into(),
            from: from.map(Value::from),
            to: to.map(Value::from),
        };
        assert!(changes.contains(&change(ChangeType::Removed, "q.a", Some(1), None)));
        assert!(changes.contains(&change(ChangeType::Changed, "q.b", Some(2), Some(3))));
        assert!(changes.contains(&change(ChangeType::Added, "q.c", None, Some(4))));
    }

    #[test]
    fn 두_요청을_쿼리_소문자_헤더_json_본문에_걸쳐_비교한다() {
        let a = request(
            "https://a.com/x?p=1",
            &[("X-Token", "a")],
            Some(r#"{"n":1}"#),
        );
        let b = request(
            "https://a.com/x?p=2",
            &[("x-token", "b")],
            Some(r#"{"n":2}"#),
        );
        let changes = summary(&diff_requests(&a, &b));
        assert!(
            changes
                .contains(&r#"{"type":"changed","key":"query.p","from":"1","to":"2"}"#.to_owned())
        );
        assert!(changes.contains(
            &r#"{"type":"changed","key":"headers.x-token","from":"a","to":"b"}"#.to_owned()
        ));
        assert!(
            changes.contains(&r#"{"type":"changed","key":"body.n","from":1,"to":2}"#.to_owned())
        );
    }

    #[test]
    fn 가장_흔한_호스트와_경로_접두사를_먼저_둔다() {
        let bases = compute_api_base(
            &[
                "https://api.x.com/v1/users/1",
                "https://api.x.com/v1/users/2",
                "https://api.x.com/v2/orders",
            ],
            API_BASE_TOP,
        );
        assert_eq!(
            bases.first(),
            Some(&ApiBase {
                base: "https://api.x.com/v1/users".into(),
                count: 2
            })
        );
    }

    // ── not in the TypeScript suite ─────────────────────────────────────────

    #[test]
    fn 쿼리를_이름별로_파싱하고_반복된_이름은_마지막_값을_쓴다() {
        // Numeric names first: the TypeScript builds a JavaScript object.
        let q = parse_query("https://a.io/x?b=1&a=2&b=3&sp=a+b%20c&10=x&2=y");
        assert_eq!(
            Value::Object(q).to_string(),
            r#"{"2":"y","10":"x","b":"3","a":"2","sp":"a b c"}"#
        );
    }

    #[test]
    fn 해석할_수_없는_url의_쿼리는_비어_있다() {
        assert!(parse_query("not a url").is_empty());
        assert!(parse_query("https://a.io/x").is_empty());
    }

    #[test]
    fn 추가_삭제_변경을_키_순서대로_낸다() {
        let changes = diff_objects(
            "query",
            &obj(json!({"same": "1", "gone": "x", "page": "1"})),
            &obj(json!({"page": "2", "new": "y", "same": "1"})),
        );
        assert_eq!(
            summary(&changes),
            [
                r#"{"type":"removed","key":"query.gone","from":"x"}"#,
                r#"{"type":"changed","key":"query.page","from":"1","to":"2"}"#,
                r#"{"type":"added","key":"query.new","to":"y"}"#,
            ]
        );
    }

    #[test]
    fn 중첩_값은_json_문자열로_비교한다() {
        let a = try_parse_json_object(Some(r#"{"n":1.0,"o":{"x":1,"y":2},"l":[1,2]}"#)).unwrap();
        let same = try_parse_json_object(Some(r#"{"n":1,"o":{"x":1,"y":2},"l":[1,2]}"#)).unwrap();
        assert_eq!(diff_objects("body", &a, &same), []);
        // Same members in another order stringify differently in JavaScript.
        let reordered =
            try_parse_json_object(Some(r#"{"n":1,"o":{"y":2,"x":1},"l":[2,1]}"#)).unwrap();
        let keys: Vec<String> = diff_objects("body", &a, &reordered)
            .into_iter()
            .map(|c| c.key)
            .collect();
        assert_eq!(keys, ["body.o", "body.l"]);
    }

    #[test]
    fn double로_같은_큰_정수는_변경으로_보지_않는다() {
        // JavaScript holds both ids as the same double; a real difference shows
        // as the rounded values it would print.
        let body = |text: &str| request("https://a.io/", &[], Some(text));
        let a = body(r#"{"id":12345678901234567890}"#);
        assert_eq!(
            diff_requests(&a, &body(r#"{"id":12345678901234567891}"#)),
            []
        );
        assert_eq!(
            summary(&diff_requests(&a, &body(r#"{"id":12345678901234599999}"#))),
            [
                r#"{"type":"changed","key":"body.id","from":12345678901234567000,"to":12345678901234600000}"#
            ]
        );
    }

    #[test]
    fn null_값도_추가로_보고하고_직렬화에_남긴다() {
        let changes = diff_objects("body", &Map::new(), &obj(json!({"a": null})));
        assert_eq!(
            summary(&changes),
            [r#"{"type":"added","key":"body.a","to":null}"#]
        );
    }

    #[test]
    fn json_객체와_배열만_객체로_파싱한다() {
        assert_eq!(try_parse_json_object(None), None);
        assert_eq!(try_parse_json_object(Some("")), None);
        assert_eq!(try_parse_json_object(Some("{oops")), None);
        assert_eq!(try_parse_json_object(Some("42")), None);
        assert_eq!(try_parse_json_object(Some("null")), None);
        assert_eq!(try_parse_json_object(Some(r#""text""#)), None);
        assert_eq!(
            try_parse_json_object(Some(r#"{"a":1}"#)),
            Some(obj(json!({"a": 1})))
        );
        // typeof [] === 'object': an array diffs by index.
        assert_eq!(
            try_parse_json_object(Some("[5,6]")),
            Some(obj(json!({"0": 5, "1": 6})))
        );
    }

    #[test]
    fn 두_요청의_쿼리_헤더_본문_차이를_한_목록으로_낸다() {
        let a = request(
            "https://a.io/x?page=1",
            &[("Accept", "*/*"), ("X-Token", "t1")],
            Some(r#"{"id":1,"tags":["a"]}"#),
        );
        let b = request(
            "https://a.io/x?page=2&sort=asc",
            &[("accept", "*/*"), ("x-token", "t2")],
            Some(r#"{"id":1,"tags":["b"],"extra":true}"#),
        );
        assert_eq!(
            summary(&diff_requests(&a, &b)),
            [
                r#"{"type":"changed","key":"query.page","from":"1","to":"2"}"#,
                r#"{"type":"added","key":"query.sort","to":"asc"}"#,
                r#"{"type":"changed","key":"headers.x-token","from":"t1","to":"t2"}"#,
                r#"{"type":"changed","key":"body.tags","from":["a"],"to":["b"]}"#,
                r#"{"type":"added","key":"body.extra","to":true}"#,
            ]
        );
    }

    #[test]
    fn json이_아닌_본문은_통째로_비교한다() {
        let a = request("https://a.io/", &[], Some("a=1"));
        let b = request("https://a.io/", &[], Some("a=2"));
        assert_eq!(
            summary(&diff_requests(&a, &b)),
            [r#"{"type":"changed","key":"body","from":"a=1","to":"a=2"}"#]
        );
        // One side JSON, the other missing: `from` is the raw text, `to` is absent.
        let json_body = request("https://a.io/", &[], Some(r#"{"a":1}"#));
        let none = request("https://a.io/", &[], None);
        assert_eq!(
            summary(&diff_requests(&json_body, &none)),
            [r#"{"type":"changed","key":"body","from":"{\"a\":1}"}"#]
        );
        assert_eq!(diff_requests(&a, &a), []);
    }

    #[test]
    fn json_배열_본문은_인덱스별로_비교한다() {
        let a = request("https://a.io/", &[], Some("[5,6]"));
        let b = request("https://a.io/", &[], Some("[5,7,8]"));
        assert_eq!(
            summary(&diff_requests(&a, &b)),
            [
                r#"{"type":"changed","key":"body.1","from":6,"to":7}"#,
                r#"{"type":"added","key":"body.2","to":8}"#,
            ]
        );
    }

    #[test]
    fn 호스트와_앞_두_경로_세그먼트로_api_베이스를_센다() {
        let urls = [
            "https://api.a.io/v1/users/1",
            "https://api.a.io/v1/users/2?x=1",
            "https://api.a.io/v1/orders",
            "https://api.a.io:8443/v1/users",
            "http://b.io/",
            "not a url",
            "data:text/plain,hi",
        ];
        let bases = compute_api_base(&urls, API_BASE_TOP);
        assert_eq!(
            serde_json::to_string(&bases).unwrap(),
            concat!(
                r#"[{"base":"https://api.a.io/v1/users","count":2},"#,
                r#"{"base":"https://api.a.io/v1/orders","count":1},"#,
                r#"{"base":"https://api.a.io:8443/v1/users","count":1},"#,
                r#"{"base":"http://b.io/","count":1},"#,
                r#"{"base":"data:///text/plain,hi","count":1}]"#
            )
        );
        assert_eq!(compute_api_base(&urls, 1).len(), 1);
        assert_eq!(compute_api_base::<&str>(&[], API_BASE_TOP), []);
    }
}
