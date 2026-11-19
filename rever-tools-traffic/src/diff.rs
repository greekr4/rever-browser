//! Port of `src/main/mcp/tools/diff.ts` — request_diff and find_api_base.

use rever_analysis::request_diff::{
    API_BASE_TOP, Change, ChangeType, compute_api_base, diff_requests,
};
use rever_core::traffic::ListFilter;
use rever_tools_api::tool::{ToolContext, ToolDef};
use rever_tools_api::utils::ToolResult;
use serde_json::json;

use crate::args::{Args, limit_prop, prop, schema};
use crate::{ok_pretty, store};

tool!(
    RequestDiff,
    ToolDef::new(
        "request_diff",
        "Compare two captured requests (URL query params, headers, post body). Shows added/removed/changed keys.",
        schema(
            json!({
                "a": { "type": "string", "description": "First requestId" },
                "b": { "type": "string", "description": "Second requestId" }
            }),
            &["a", "b"],
        ),
    ),
    |ctx, args| request_diff(ctx, &args)
);

fn request_diff(ctx: &ToolContext, args: &Args) -> Result<ToolResult, String> {
    let a = args.required_str("a")?;
    let b = args.required_str("b")?;
    let store = store(ctx);
    let Some(req_a) = store.get(a) else {
        return Err(format!("unknown requestId: {a}"));
    };
    let Some(req_b) = store.get(b) else {
        return Err(format!("unknown requestId: {b}"));
    };

    let changes = diff_requests(req_a, req_b);
    let of =
        |kind: ChangeType| -> Vec<&Change> { changes.iter().filter(|c| c.kind == kind).collect() };
    ok_pretty(&json!({
        "a": { "requestId": a, "url": req_a.url, "method": req_a.method },
        "b": { "requestId": b, "url": req_b.url, "method": req_b.method },
        "changes": {
            "added": of(ChangeType::Added),
            "removed": of(ChangeType::Removed),
            "changed": of(ChangeType::Changed)
        }
    }))
}

tool!(
    FindApiBase,
    ToolDef::new(
        "find_api_base",
        "Analyze captured XHR/Fetch requests and find the most common host+path-prefix combinations — useful to identify the API base URL.",
        schema(
            json!({
                "host": prop("string", "Restrict to requests from this host substring"),
                "limit": limit_prop("Max requests to analyze (default 200)", 500)
            }),
            &[],
        ),
    ),
    |ctx, args| find_api_base(ctx, &args)
);

fn find_api_base(ctx: &ToolContext, args: &Args) -> Result<ToolResult, String> {
    let host = args.str("host")?;
    let limit = args.limit(500)?.unwrap_or(200);
    let store = store(ctx);
    let of = |kind: &str| {
        store.list(&ListFilter {
            host: host.map(str::to_owned),
            method_or_type: Some(kind.to_owned()),
            limit: Some(limit),
            ..ListFilter::default()
        })
    };
    let mut all = of("Fetch");
    all.extend(of("XHR"));

    let urls: Vec<&str> = all.iter().map(|r| r.url.as_str()).collect();
    let candidates = compute_api_base(&urls, API_BASE_TOP);
    ok_pretty(&json!({ "totalAnalyzed": all.len(), "candidates": candidates }))
}

#[cfg(test)]
mod tests {
    use rever_tools_api::tool::ToolContext;

    use super::*;
    use crate::test_support::{call, call_json, put, request};

    #[tokio::test]
    async fn 두_요청의_차이를_추가_삭제_변경으로_나눠_보여준다() {
        let ctx = ToolContext::for_tests();
        let mut first = request("1", "https://a.test/api?page=1&q=x", "POST", "XHR");
        first.request_post_data = Some(r#"{"name":"kim","age":1}"#.to_owned());
        put(&ctx, first, 100);
        let mut second = request("2", "https://a.test/api?page=2&sort=asc", "POST", "XHR");
        second.request_post_data = Some(r#"{"name":"lee","age":1}"#.to_owned());
        put(&ctx, second, 200);

        let text = call(&RequestDiff, &ctx, json!({ "a": "1", "b": "2" }))
            .await
            .unwrap();
        assert_eq!(
            text,
            r#"{
  "a": {
    "requestId": "1",
    "url": "https://a.test/api?page=1&q=x",
    "method": "POST"
  },
  "b": {
    "requestId": "2",
    "url": "https://a.test/api?page=2&sort=asc",
    "method": "POST"
  },
  "changes": {
    "added": [
      {
        "type": "added",
        "key": "query.sort",
        "to": "asc"
      }
    ],
    "removed": [
      {
        "type": "removed",
        "key": "query.q",
        "from": "x"
      }
    ],
    "changed": [
      {
        "type": "changed",
        "key": "query.page",
        "from": "1",
        "to": "2"
      },
      {
        "type": "changed",
        "key": "body.name",
        "from": "kim",
        "to": "lee"
      }
    ]
  }
}"#
        );
    }

    #[tokio::test]
    async fn 같은_요청끼리는_차이가_없다() {
        let ctx = ToolContext::for_tests();
        put(&ctx, request("1", "https://a.test/api", "GET", "XHR"), 100);
        let out = call_json(&RequestDiff, &ctx, json!({ "a": "1", "b": "1" })).await;
        assert_eq!(
            out["changes"],
            json!({ "added": [], "removed": [], "changed": [] })
        );
    }

    #[tokio::test]
    async fn 모르는_요청은_a부터_확인해_알린다() {
        let ctx = ToolContext::for_tests();
        put(&ctx, request("1", "https://a.test/api", "GET", "XHR"), 100);
        assert_eq!(
            call(&RequestDiff, &ctx, json!({ "a": "x", "b": "y" })).await,
            Err("unknown requestId: x".to_owned())
        );
        assert_eq!(
            call(&RequestDiff, &ctx, json!({ "a": "1", "b": "y" })).await,
            Err("unknown requestId: y".to_owned())
        );
        assert!(call(&RequestDiff, &ctx, json!({ "a": "1" })).await.is_err());
    }

    #[tokio::test]
    async fn xhr과_fetch_요청에서_가장_흔한_api_기준_주소를_찾는다() {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("1", "https://api.a.test/v1/users/1", "GET", "Fetch"),
            100,
        );
        put(
            &ctx,
            request("2", "https://api.a.test/v1/users/2", "GET", "XHR"),
            200,
        );
        put(
            &ctx,
            request("3", "https://api.a.test/v2/items", "GET", "XHR"),
            300,
        );
        put(
            &ctx,
            request("4", "https://b.test/v1/users", "GET", "Fetch"),
            400,
        );
        // Not an API call: left out.
        put(
            &ctx,
            request("5", "https://api.a.test/v1/users/app.js", "GET", "Script"),
            500,
        );

        let text = call(&FindApiBase, &ctx, json!({})).await.unwrap();
        assert_eq!(
            text,
            r#"{
  "totalAnalyzed": 4,
  "candidates": [
    {
      "base": "https://api.a.test/v1/users",
      "count": 2
    },
    {
      "base": "https://b.test/v1/users",
      "count": 1
    },
    {
      "base": "https://api.a.test/v2/items",
      "count": 1
    }
  ]
}"#
        );

        let by_host = call_json(&FindApiBase, &ctx, json!({ "host": "b.test" })).await;
        assert_eq!(by_host["totalAnalyzed"], 1);
        // The limit applies to each of the two scans.
        let limited = call_json(&FindApiBase, &ctx, json!({ "limit": 1 })).await;
        assert_eq!(limited["totalAnalyzed"], 2);
        assert!(
            call(&FindApiBase, &ctx, json!({ "limit": 501 }))
                .await
                .is_err()
        );
    }
}
