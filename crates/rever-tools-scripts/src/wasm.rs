//! Port of `src/main/mcp/tools/wasm.ts` — list_wasm, wasm_decompile,
//! wasm_info, grep_wasm and wasm_xref over the `.wasm` bodies already in the
//! traffic store.

use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Value, json};

use crate::args::Args;
use crate::wasm_analysis::{
    StringsOptions, WasmFormat, body_byte_length, decompile_request, extract_wasm_strings,
    get_wasm_buffer, list_wasm, wasm_objdump, xref_exports,
};
use crate::{ok_pretty, traffic};

pub struct ListWasm;

impl Tool for ListWasm {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "list_wasm",
            "List captured WebAssembly modules (application/wasm bodies or .wasm URLs) from the traffic store, largest first. Use this to find the requestId to pass to the other wasm_* tools.",
            json!({
                "type": "object",
                "properties": {
                    "host": { "type": "string", "description": "Only modules whose host contains this" }
                }
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { list(ctx, &args).unwrap_or_else(err) })
    }
}

fn list(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("list_wasm", args)?;
    let host = args.opt_str("host")?;
    let store = traffic(ctx);
    let rows: Vec<Value> = list_wasm(&store, host)
        .into_iter()
        .map(|r| {
            json!({
                "requestId": r.request_id,
                "url": r.url,
                "bytes": body_byte_length(r),
                "captured": r.response_body.as_deref().is_some_and(|b| !b.is_empty()),
                "truncated": r.response_body_truncated == Some(true),
            })
        })
        .collect();
    if rows.is_empty() {
        return Ok(ok(
            "No WASM modules captured yet. Load the page that fetches the .wasm, then retry.",
        ));
    }
    Ok(ok_pretty(&rows))
}

pub struct WasmDecompile;

impl Tool for WasmDecompile {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "wasm_decompile",
            "Disassemble a captured WebAssembly module to text. `format:\"wat\"` (default) is the guaranteed in-process baseline; `\"decompile\"` is a higher-level C-like view (wasm-decompile); `\"c\"` is full C source (wasm2c), the most readable for complex crypto/signing routines. Non-WAT formats degrade to WAT if unavailable. Get the requestId from list_wasm.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": { "type": "string", "description": "requestId of a captured WASM module (from list_wasm)" },
                    "format": {
                        "type": "string",
                        "enum": ["wat", "decompile", "c"],
                        "description": "wat (default) | decompile (C-like) | c (full C source via wasm2c)"
                    },
                    "full": { "type": "boolean", "description": "deprecated alias: full:true == format:\"decompile\"" }
                },
                "required": ["requestId"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { decompile(ctx, &args).await.unwrap_or_else(err) })
    }
}

async fn decompile(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("wasm_decompile", args)?;
    let request_id = args.str("requestId")?;
    let format = args.opt_enum("format", &["wat", "decompile", "c"])?;
    let full = args.opt_bool("full")?.unwrap_or(false);
    let format = match format {
        Some("c") => WasmFormat::C,
        Some("decompile") => WasmFormat::Decompile,
        Some(_) => WasmFormat::Wat,
        None if full => WasmFormat::Decompile,
        None => WasmFormat::Wat,
    };
    Ok(decompile_request(&ctx.traffic, request_id, format).await)
}

pub struct WasmInfo;

impl Tool for WasmInfo {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "wasm_info",
            "Summarize a captured WASM module (wasm-objdump -x): its types, imports, exports, and — when a name section is present — the real function/symbol names. Use this first to see the export surface and pick which function to decompile.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": { "type": "string", "description": "requestId of a captured WASM module (from list_wasm)" }
                },
                "required": ["requestId"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { info(ctx, &args).unwrap_or_else(err) })
    }
}

fn info(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("wasm_info", args)?;
    let bytes = get_wasm_buffer(&traffic(ctx), args.str("requestId")?)?;
    wasm_objdump(&bytes).map(ok)
}

pub struct GrepWasm;

impl Tool for GrepWasm {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "grep_wasm",
            "Extract printable strings/constants from a captured WASM body (a `strings`-style scan of the data section): algorithm ids, embedded keys, URLs, symbol names. Optionally filter by a regex. Fast way to spot signing logic without reading the whole disassembly.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": { "type": "string", "description": "requestId of a captured WASM module (from list_wasm)" },
                    "pattern": { "type": "string", "description": "Only strings matching this JS regex source" },
                    "min": { "type": "number", "description": "Minimum run length (default 4)" }
                },
                "required": ["requestId"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { grep(ctx, &args).unwrap_or_else(err) })
    }
}

fn grep(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("grep_wasm", args)?;
    let request_id = args.str("requestId")?;
    let opts = StringsOptions {
        pattern: args.opt_str("pattern")?,
        min: args.opt_number("min")?,
    };
    let bytes = get_wasm_buffer(&traffic(ctx), request_id)?;
    let strings = extract_wasm_strings(&bytes, opts)?;
    if strings.is_empty() {
        return Ok(ok("No matching strings found."));
    }
    Ok(ok_pretty(&strings))
}

pub struct WasmXref;

impl Tool for WasmXref {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "wasm_xref",
            "Cross-reference a WASM module's exported functions against captured JS: for each export, grep the script bodies for its name and show where it is called. Bridges the WASM ↔ JS gap — e.g. finding the JS call site that invokes a WASM signing export.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": { "type": "string", "description": "requestId of a captured WASM module (from list_wasm)" }
                },
                "required": ["requestId"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { xref(ctx, &args).unwrap_or_else(err) })
    }
}

fn xref(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("wasm_xref", args)?;
    let res = xref_exports(&traffic(ctx), args.str("requestId")?)?;
    Ok(ok_pretty(&res))
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ListWasm);
    registry.register(WasmDecompile);
    registry.register(WasmInfo);
    registry.register(GrepWasm);
    registry.register(WasmXref);
}

#[cfg(test)]
mod tests {
    use rever_core::traffic::RequestPatch;
    use rever_tools_api::utils::Content;

    use super::*;

    // The 46-byte module exporting "checksum" from wasm-analysis.test.ts.
    const CHECKSUM_WASM_B64: &str =
        "AGFzbQEAAAABBwFgAn9/AX8DAgEABwwBCGNoZWNrc3VtAAAKCQEHACAAIAFqCw==";

    fn wasm(id: &str, url: &str, body: Option<&str>) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            url: Some(url.into()),
            mime_type: Some("application/wasm".into()),
            response_body: body.map(str::to_owned),
            response_body_base64: body.map(|_| true),
            ..RequestPatch::default()
        }
    }

    fn context(patches: Vec<RequestPatch>) -> ToolContext {
        let ctx = ToolContext::for_tests();
        for patch in patches {
            traffic(&ctx).upsert(patch, 1);
        }
        ctx
    }

    fn checksum_context() -> ToolContext {
        context(vec![
            wasm("w1", "http://h/sign.wasm", Some(CHECKSUM_WASM_B64)),
            RequestPatch {
                request_id: "app".into(),
                url: Some("http://h/app.js".into()),
                resource_type: Some("Script".into()),
                response_body: Some("const r = instance.exports.checksum(2, 3)".into()),
                ..RequestPatch::default()
            },
        ])
    }

    async fn call(tool: impl Tool, ctx: &ToolContext, args: Value) -> (bool, String) {
        let result = tool.call(ctx, args).await;
        let text = match result.content.first() {
            Some(Content::Text { text }) => text.clone(),
            None => String::new(),
        };
        (result.is_error, text)
    }

    #[tokio::test]
    async fn 캡처된_모듈을_크기와_상태와_함께_나열한다() {
        let ctx = context(vec![
            wasm("w1", "http://a.test/sign.wasm", Some(CHECKSUM_WASM_B64)),
            wasm("pending", "http://b.test/later.wasm", None),
            RequestPatch {
                response_body_truncated: Some(true),
                ..wasm("cut", "http://b.test/big.wasm", Some("AGFzbQEAAAA="))
            },
        ]);
        assert_eq!(
            call(ListWasm, &ctx, json!({})).await,
            (
                false,
                r#"[
  {
    "requestId": "w1",
    "url": "http://a.test/sign.wasm",
    "bytes": 48,
    "captured": true,
    "truncated": false
  },
  {
    "requestId": "cut",
    "url": "http://b.test/big.wasm",
    "bytes": 9,
    "captured": true,
    "truncated": true
  },
  {
    "requestId": "pending",
    "url": "http://b.test/later.wasm",
    "bytes": 0,
    "captured": false,
    "truncated": false
  }
]"#
                .to_owned()
            )
        );
        let (_, text) = call(ListWasm, &ctx, json!({ "host": "a.test" })).await;
        let rows: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(rows.as_array().map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn 캡처된_모듈이_없으면_안내_문구를_돌려준다() {
        let expected = (
            false,
            "No WASM modules captured yet. Load the page that fetches the .wasm, then retry."
                .to_owned(),
        );
        assert_eq!(call(ListWasm, &context(vec![]), json!({})).await, expected);
        assert_eq!(
            call(ListWasm, &checksum_context(), json!({ "host": "nowhere" })).await,
            expected
        );
        assert!(
            call(ListWasm, &context(vec![]), json!({ "host": 1 }))
                .await
                .0
        );
    }

    #[tokio::test]
    async fn 모듈을_기본으로_wat로_풀어_준다() {
        let ctx = checksum_context();
        let (is_error, wat) = call(WasmDecompile, &ctx, json!({ "requestId": "w1" })).await;
        assert!(!is_error);
        assert!(wat.starts_with("(module"), "{wat}");
        assert!(wat.contains(r#"(export "checksum" (func 0))"#), "{wat}");
        assert_eq!(
            call(
                WasmDecompile,
                &ctx,
                json!({ "requestId": "w1", "format": "wat", "full": true })
            )
            .await
            .1,
            wat
        );
    }

    #[tokio::test]
    async fn wat가_아닌_형식은_만들_수_없어도_에러가_아니다() {
        let ctx = checksum_context();
        for args in [
            json!({ "requestId": "w1", "format": "decompile" }),
            json!({ "requestId": "w1", "format": "c" }),
            json!({ "requestId": "w1", "full": true }),
        ] {
            let (is_error, text) = call(WasmDecompile, &ctx, args).await;
            assert!(!is_error, "{text}");
            assert!(!text.is_empty());
        }
    }

    #[tokio::test]
    async fn 잘못된_인자와_모르는_요청은_에러다() {
        let ctx = checksum_context();
        assert!(
            call(
                WasmDecompile,
                &ctx,
                json!({ "requestId": "w1", "format": "asm" })
            )
            .await
            .0
        );
        assert!(
            call(
                WasmDecompile,
                &ctx,
                json!({ "requestId": "w1", "full": "yes" })
            )
            .await
            .0
        );
        assert!(
            call(GrepWasm, &ctx, json!({ "requestId": "w1", "min": "4" }))
                .await
                .0
        );
        for tool in [&WasmDecompile as &dyn Tool, &WasmInfo, &GrepWasm, &WasmXref] {
            assert!(tool.call(&ctx, json!({})).await.is_error);
            let result = tool.call(&ctx, json!({ "requestId": "nope" })).await;
            assert_eq!(result, err("unknown requestId: nope"));
        }
    }

    #[tokio::test]
    async fn 모듈_요약은_타입과_export를_보여주고_깨진_모듈은_에러다() {
        let ctx = context(vec![
            wasm("w1", "http://h/sign.wasm", Some(CHECKSUM_WASM_B64)),
            wasm("bad", "http://h/bad.wasm", Some("AAAA")),
        ]);
        let (is_error, text) = call(WasmInfo, &ctx, json!({ "requestId": "w1" })).await;
        assert!(!is_error);
        assert!(text.contains(" - type[0] (i32, i32) -> i32\n"), "{text}");
        assert!(
            text.contains(" - func[0] <checksum> -> \"checksum\"\n"),
            "{text}"
        );
        assert!(call(WasmInfo, &ctx, json!({ "requestId": "bad" })).await.0);
    }

    #[tokio::test]
    async fn 본문의_문자열을_찾고_패턴과_최소_길이로_거른다() {
        let ctx = checksum_context();
        assert_eq!(
            call(GrepWasm, &ctx, json!({ "requestId": "w1" })).await,
            (false, "[\n  \"checksum\"\n]".to_owned())
        );
        assert_eq!(
            call(
                GrepWasm,
                &ctx,
                json!({ "requestId": "w1", "pattern": "^check", "min": 8 })
            )
            .await,
            (false, "[\n  \"checksum\"\n]".to_owned())
        );
        for args in [
            json!({ "requestId": "w1", "pattern": "hmac" }),
            json!({ "requestId": "w1", "min": 9 }),
        ] {
            assert_eq!(
                call(GrepWasm, &ctx, args).await,
                (false, "No matching strings found.".to_owned())
            );
        }
        let (is_error, text) =
            call(GrepWasm, &ctx, json!({ "requestId": "w1", "pattern": "(" })).await;
        assert!(is_error);
        assert!(
            text.starts_with("Invalid regular expression: /(/: "),
            "{text}"
        );
    }

    #[tokio::test]
    async fn export를_호출하는_js_위치를_돌려준다() {
        assert_eq!(
            call(WasmXref, &checksum_context(), json!({ "requestId": "w1" })).await,
            (
                false,
                r#"[
  {
    "export": "checksum",
    "referenced": true,
    "hits": [
      {
        "requestId": "app",
        "url": "http://h/app.js",
        "snippet": "const r = instance.exports.checksum(2, 3)"
      }
    ]
  }
]"#
                .to_owned()
            )
        );
        let ctx = context(vec![wasm("bad", "http://h/bad.wasm", Some("AAAA"))]);
        assert!(call(WasmXref, &ctx, json!({ "requestId": "bad" })).await.0);
    }
}
