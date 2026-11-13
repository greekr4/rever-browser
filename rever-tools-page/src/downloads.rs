//! Port of `src/main/mcp/tools/downloads.ts` — the files the page downloaded.
//!
//! The TypeScript read a download list its Electron session kept. `Browser` does not
//! report downloads, so both tools check their arguments and then say so; an empty
//! list would claim that nothing was downloaded.

use rever_tools_api::tool::{ToolDef, ToolRegistry, unsupported};
use serde_json::json;

use crate::schema;

const DOWNLOAD_TRACKING: &str = "download tracking";

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ListDownloads);
    registry.register(WaitForDownload);
}

tool!(
    ListDownloads,
    ToolDef::new(
        "list_downloads",
        "List files the page downloaded (newest last): filename, url, mime, bytes, state (progressing/completed/cancelled/interrupted) and savePath. Downloads are saved to ~/Downloads/Rever without a save dialog unless the user turned on \"ask where to save\".",
        schema(
            json!({
                "limit": {
                    "description": "Default 20",
                    "type": "integer",
                    "exclusiveMinimum": 0,
                    "maximum": 100
                }
            }),
            &[]
        )
    ),
    |_ctx, _args| { Err(unsupported(DOWNLOAD_TRACKING)) }
);

tool!(
    WaitForDownload,
    ToolDef::new(
        "wait_for_download",
        "Wait until a download finishes and return it (savePath, bytes, state). Call right after the click that starts a download. Returns a download that finished in the last 30s if you have not seen it yet, so calling it after a fast download is fine. Each download is returned once.",
        schema(
            json!({
                "timeoutMs": {
                    "description": "Default 30000",
                    "type": "integer",
                    "exclusiveMinimum": 0,
                    "maximum": 120000
                }
            }),
            &[]
        )
    ),
    |_ctx, _args| { Err(unsupported(DOWNLOAD_TRACKING)) }
);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{
        FakeBrowser, call, context, invalid, not_int, rejected, too_big, too_small,
    };

    #[tokio::test]
    async fn 다운로드_목록은_인자를_검사한_뒤_지원하지_않는다고_알린다() {
        let (_, ctx) = context(FakeBrowser::default());
        for (limit, issue) in [
            (json!(0), too_small("limit")),
            (json!(101), too_big("limit", 100)),
            (json!(2.5), not_int("limit")),
        ] {
            assert_eq!(
                call(&ListDownloads, &ctx, json!({ "limit": limit })).await,
                rejected("list_downloads", json!([issue]))
            );
        }
        assert_eq!(
            call(&ListDownloads, &ctx, json!({ "limit": "20" })).await,
            invalid("list_downloads", "limit", "number", "string")
        );
        for args in [json!({}), json!({ "limit": 100 })] {
            assert_eq!(
                call(&ListDownloads, &ctx, args).await,
                Err(unsupported("download tracking"))
            );
        }
    }

    #[tokio::test]
    async fn 다운로드_대기는_인자를_검사한_뒤_지원하지_않는다고_알린다() {
        let (_, ctx) = context(FakeBrowser::default());
        for (timeout, issue) in [
            (json!(0), too_small("timeoutMs")),
            (json!(120_001), too_big("timeoutMs", 120_000)),
            (json!(-1), too_small("timeoutMs")),
        ] {
            assert_eq!(
                call(&WaitForDownload, &ctx, json!({ "timeoutMs": timeout })).await,
                rejected("wait_for_download", json!([issue]))
            );
        }
        // It answers at once instead of waiting out the timeout.
        for args in [json!({}), json!({ "timeoutMs": 120_000 })] {
            assert_eq!(
                call(&WaitForDownload, &ctx, args).await,
                Err(unsupported("download tracking"))
            );
        }
    }
}
