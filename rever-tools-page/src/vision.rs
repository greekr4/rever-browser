//! Port of `src/main/mcp/tools/vision.ts` — a screenshot of the current page judged
//! by a vision model.
//!
//! Takes a screenshot and lets a vision model judge what the accessibility tree
//! (browser_snapshot) cannot show: layout, popups and modals, captchas, ad overlays,
//! "did the search really run", "did results appear". Claude when an Anthropic key is
//! set, OpenAI otherwise.
//!
//! The TypeScript called the model through the Anthropic and OpenAI SDKs with a key
//! from the app's settings. Tools have neither an API client nor the key store, so
//! the tool checks its arguments and reports the call as unsupported. The models and
//! the prompt are kept for whoever wires a client in.

use rever_tools_api::tool::{ToolDef, ToolRegistry, unsupported};
use serde_json::json;

use crate::schema;

pub const ANTHROPIC_VISION_MODEL: &str = "claude-sonnet-5";
pub const OPENAI_VISION_MODEL: &str = "gpt-4o";

pub const SYSTEM_PROMPT: &str = concat!(
    "You are a vision judge for a browser-automation agent. You are shown a screenshot of ",
    "the current web page. Answer the question precisely and concisely, describing only what ",
    "is actually visible. If asked where an element is or how to interact with it, describe its ",
    "visible location and, when you can reasonably infer it, suggest a CSS selector. If asked a ",
    "yes/no judgement (e.g. did the search run, is a modal blocking the page), answer with a clear ",
    "yes/no plus the visual evidence. Never invent content that is not on screen."
);

pub fn register(registry: &mut ToolRegistry) {
    registry.register(VisionJudge);
}

tool!(
    VisionJudge,
    ToolDef::new(
        "vision_judge",
        "Capture a screenshot of the current page and ask a vision model to judge it. Use for things the accessibility snapshot cannot show: whether an action visually succeeded (did the search run? did results render?), whether a modal/captcha/ad overlay is blocking the page, reading text baked into images/canvas, or locating an element visually. Returns the model's answer as text. Uses Claude if an Anthropic key is set, otherwise OpenAI.",
        schema(
            json!({
                "question": {
                    "type": "string",
                    "description": "What to decide from the screenshot, e.g. \"Did the search for 오늘 날씨 actually run and show results?\" or \"Is anything covering the search box?\""
                },
                "model": {
                    "description": "Optional vision model override for the selected provider (default: claude-sonnet-5 for Anthropic, gpt-4o for OpenAI).",
                    "type": "string"
                }
            }),
            &["question"]
        )
    ),
    |_ctx, _args| { Err(unsupported("vision model calls")) }
);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{FakeBrowser, call, context, invalid};

    #[tokio::test]
    async fn 질문이_없거나_인자_타입이_틀리면_거절한다() {
        let (_, ctx) = context(FakeBrowser::default());
        assert_eq!(
            call(&VisionJudge, &ctx, json!({})).await,
            invalid("vision_judge", "question", "string", "undefined")
        );
        assert_eq!(
            call(&VisionJudge, &ctx, json!({ "question": ["a"] })).await,
            invalid("vision_judge", "question", "string", "array")
        );
        assert_eq!(
            call(&VisionJudge, &ctx, json!({ "question": "q", "model": 4 })).await,
            invalid("vision_judge", "model", "string", "number")
        );
    }

    #[tokio::test]
    async fn 모델_호출은_지원하지_않는다고_알리고_스크린샷도_찍지_않는다() {
        let (browser, ctx) = context(FakeBrowser::default());
        for args in [
            json!({ "question": "Is a modal open?" }),
            json!({ "question": "Is a modal open?", "model": "gpt-4o" }),
        ] {
            assert_eq!(
                call(&VisionJudge, &ctx, args).await,
                Err(unsupported("vision model calls"))
            );
        }
        assert!(browser.calls().is_empty());
    }

    #[test]
    fn 프롬프트와_기본_모델은_타입스크립트와_같다() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/main/mcp/tools/vision.ts"
        );
        let Ok(source) = std::fs::read_to_string(path) else {
            eprintln!("skipped: {path} is not there");
            return;
        };
        assert!(source.contains(&format!(
            "ANTHROPIC_VISION_MODEL = '{ANTHROPIC_VISION_MODEL}'"
        )));
        assert!(source.contains(&format!("OPENAI_VISION_MODEL = '{OPENAI_VISION_MODEL}'")));
        // The prompt is six string literals joined with `+`, none of them escaped.
        let pieces: Vec<&str> = source
            .split_once("const SYSTEM_PROMPT =")
            .and_then(|(_, rest)| rest.split_once("\n\n"))
            .map(|(literals, _)| literals.split('\'').skip(1).step_by(2).collect())
            .unwrap_or_default();
        assert_eq!(pieces.len(), 6);
        assert_eq!(SYSTEM_PROMPT, pieces.concat());
    }
}
