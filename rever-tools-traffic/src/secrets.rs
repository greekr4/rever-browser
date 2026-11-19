//! Port of `src/main/mcp/tools/secrets.ts` — scan_secrets.

use rever_core::secret_scan::scan_request;
use rever_core::traffic::ListFilter;
use rever_tools_api::tool::{ToolContext, ToolDef};
use rever_tools_api::utils::ToolResult;
use serde_json::{Value, json};

use crate::args::{Args, prop, schema};
use crate::{ok_pretty, store};

tool!(
    ScanSecrets,
    ToolDef::new(
        "scan_secrets",
        "Sweep captured traffic for embedded credentials and secrets — JWTs, API keys (Google/AWS/GitHub/Slack/Stripe), private keys, Bearer tokens, and api_key/secret/password assignments — across response bodies, request bodies, and sensitive request headers (Authorization/Cookie/x-api-key…). Values are masked (first/last 4 chars). A quick way to find a hardcoded key or endpoint that shortcuts the whole reversing effort. Filter by host / since to scope the sweep.",
        schema(
            json!({
                "host": prop("string", "Substring host filter (e.g. \"example.com\")"),
                "since": prop("number", "Only scan requests started after this epoch ms")
            }),
            &[],
        ),
    ),
    |ctx, args| scan_secrets(ctx, &args)
);

fn scan_secrets(ctx: &ToolContext, args: &Args) -> Result<ToolResult, String> {
    let filter = ListFilter {
        host: args.str("host")?.map(str::to_owned),
        since: args.since()?,
        limit: Some(200),
        ..ListFilter::default()
    };
    let store = store(ctx);
    let rows = store.list(&filter);
    let mut total = 0;
    let mut results: Vec<Value> = Vec::new();
    for e in &rows {
        let hits = scan_request(e);
        if hits.is_empty() {
            continue;
        }
        total += hits.len();
        results.push(json!({ "requestId": e.request_id, "url": e.url, "hits": hits }));
    }
    ok_pretty(&json!({
        "scanned": rows.len(),
        "requestsWithSecrets": results.len(),
        "totalHits": total,
        "results": results
    }))
}

#[cfg(test)]
mod tests {
    use rever_tools_api::tool::ToolContext;

    use super::*;
    use crate::test_support::{call, call_json, put, request};

    #[tokio::test]
    async fn 비밀이_없으면_훑은_개수만_알린다() {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("1", "https://a.test/", "GET", "Document"),
            100,
        );
        assert_eq!(
            call(&ScanSecrets, &ctx, json!({})).await.unwrap(),
            "{\n  \"scanned\": 1,\n  \"requestsWithSecrets\": 0,\n  \"totalHits\": 0,\n  \"results\": []\n}"
        );
    }

    #[tokio::test]
    async fn 본문과_민감한_헤더의_비밀을_가려서_요청별로_모은다() {
        let ctx = ToolContext::for_tests();
        put(
            &ctx,
            request("clean", "https://a.test/clean", "GET", "XHR"),
            100,
        );
        let mut leaky = request("leaky", "https://api.a.test/config", "GET", "XHR");
        leaky.response_body = Some("key=AKIAIOSFODNN7EXAMPLE".to_owned());
        leaky.request_headers = Some(
            [(
                "authorization".to_owned(),
                "Bearer abcdefghijklmnop".to_owned(),
            )]
            .into_iter()
            .collect(),
        );
        put(&ctx, leaky, 200);

        let out = call_json(&ScanSecrets, &ctx, json!({})).await;
        assert_eq!(out["scanned"], 2);
        assert_eq!(out["requestsWithSecrets"], 1);
        let result = &out["results"][0];
        assert_eq!(result["requestId"], "leaky");
        assert_eq!(result["url"], "https://api.a.test/config");
        let hits = result["hits"].as_array().expect("hits");
        assert_eq!(out["totalHits"], hits.len());
        assert!(hits.len() >= 2, "{hits:?}");
        for hit in hits {
            let keys: Vec<&String> = hit.as_object().expect("hit").keys().collect();
            assert_eq!(keys, ["type", "where", "masked"]);
            // Masked: the secret itself is never in the output.
            let masked = hit["masked"].as_str().expect("masked");
            assert!(!masked.contains("IOSFODNN7"), "{masked}");
            assert!(!masked.contains("efghijkl"), "{masked}");
        }
        assert!(hits.iter().any(|hit| hit["where"] == "response-body"));
    }

    #[tokio::test]
    async fn host와_since로_훑을_범위를_좁힌다() {
        let ctx = ToolContext::for_tests();
        put(&ctx, request("1", "https://a.test/", "GET", "XHR"), 100);
        put(&ctx, request("2", "https://b.test/", "GET", "XHR"), 200);
        put(&ctx, request("3", "https://b.test/", "GET", "XHR"), 300);

        let by_host = call_json(&ScanSecrets, &ctx, json!({ "host": "b.test" })).await;
        assert_eq!(by_host["scanned"], 2);
        let since = call_json(&ScanSecrets, &ctx, json!({ "since": 250 })).await;
        assert_eq!(since["scanned"], 1);
        assert!(
            call(&ScanSecrets, &ctx, json!({ "since": "yesterday" }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn 최근_200개까지만_훑는다() {
        let ctx = ToolContext::for_tests();
        for n in 0..210_u64 {
            put(
                &ctx,
                request(&n.to_string(), "https://a.test/", "GET", "XHR"),
                n,
            );
        }
        let out = call_json(&ScanSecrets, &ctx, json!({})).await;
        assert_eq!(out["scanned"], 200);
    }
}
