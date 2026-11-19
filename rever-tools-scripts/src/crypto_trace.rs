//! Port of `src/main/mcp/tools/crypto_trace.ts` — crypto_trace_start,
//! crypto_trace_list and crypto_trace_stop: record the page's Web Crypto calls
//! with an in-page hook, no debugger needed.

use std::sync::Mutex;

use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Map, Value, json};

use crate::js::utf16_len;
use crate::ok_pretty;

// In-page hook: wraps Web Crypto (crypto.subtle.*) so every importKey / sign /
// verify / digest / encrypt call records its inputs and outputs into a
// window.__revCrypto buffer. No debugger pause needed — this reveals the HMAC
// key, the signed message, and the resulting signature at call time. Guarded
// against double-install; stops pushing once the buffer is deleted.
const HOOK: &str = r#"(() => {
  if (window.__revCrypto) return 'already-installed';
  window.__revCrypto = [];
  var MAX = 200;
  function preview(data) {
    try {
      if (data == null) return null;
      if (typeof data === 'string')
        return data.length > 160 ? data.slice(0, 160) + '… (' + data.length + ' chars)' : data;
      var u8;
      if (data instanceof ArrayBuffer) u8 = new Uint8Array(data);
      else if (ArrayBuffer.isView(data)) u8 = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
      else if (typeof data === 'object') return JSON.stringify(data).slice(0, 200);
      else return String(data);
      var slice = u8.slice(0, 96);
      var bytes = Array.prototype.slice.call(slice);
      var printable = bytes.every(function (b) { return b === 9 || b === 10 || b === 13 || (b >= 32 && b < 127); });
      var repr = printable
        ? new TextDecoder().decode(slice)
        : bytes.map(function (b) { return b.toString(16).padStart(2, '0'); }).join('');
      if (u8.length > 96) repr += '…';
      return repr + ' (' + u8.length + ' bytes)';
    } catch (e) { return '(unreadable)'; }
  }
  function stackOf() {
    return (new Error().stack || '').split('\n').slice(3, 8).map(function (l) { return l.trim(); });
  }
  function push(rec) {
    var b = window.__revCrypto;
    if (!b || b.length >= MAX) return;
    rec.t = Date.now();
    b.push(rec);
  }
  var S = window.crypto && window.crypto.subtle;
  if (S) {
    function wrap(name, argFn, captureResult) {
      var orig = S[name];
      if (typeof orig !== 'function') return;
      S[name] = function () {
        var args = arguments;
        try { push(Object.assign({ op: 'subtle.' + name, stack: stackOf() }, argFn(args))); } catch (e) {}
        var p = orig.apply(this, args);
        if (captureResult && p && typeof p.then === 'function') {
          return p.then(function (r) {
            try { push({ op: 'subtle.' + name + ':result', result: preview(r), stack: [] }); } catch (e) {}
            return r;
          });
        }
        return p;
      };
    }
    wrap('importKey', function (a) { return { format: a[0], keyMaterial: preview(a[1]), algorithm: JSON.stringify(a[2]) }; }, false);
    wrap('sign', function (a) { return { algorithm: JSON.stringify(a[0]), message: preview(a[2]) }; }, true);
    wrap('verify', function (a) { return { algorithm: JSON.stringify(a[0]), message: preview(a[3]) }; }, false);
    wrap('digest', function (a) { return { algorithm: JSON.stringify(a[0]), message: preview(a[1]) }; }, true);
    wrap('encrypt', function (a) { return { algorithm: JSON.stringify(a[0]), message: preview(a[2]) }; }, true);
  }
  return 'installed';
})();"#;

/// The id of the init script that reinstalls the hook on every new document
/// (the TypeScript's module-level `persistentScriptId`).
#[derive(Default)]
pub struct CryptoTraceState {
    persistent_script_id: Mutex<Option<String>>,
}

fn no_arguments() -> Value {
    json!({ "type": "object", "properties": {} })
}

pub struct CryptoTraceStart;

impl Tool for CryptoTraceStart {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "crypto_trace_start",
            "Instrument the page Web Crypto API without a debugger: wraps crypto.subtle importKey / sign / verify / digest / encrypt so their inputs and outputs are recorded. Reveals the HMAC/AES secret key (from importKey), the signed message, and the resulting signature at call time — the pause-free way to recover how a request is signed. Install it, then trigger the signing action, then crypto_trace_list. Only catches Web Crypto (and misses hand-rolled pure-JS crypto).",
            no_arguments(),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, _: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { start(ctx).await.unwrap_or_else(err) })
    }
}

async fn start(ctx: &ToolContext) -> Result<ToolResult, String> {
    // Survive reloads / run before page scripts on the next navigation.
    let identifier = ctx.browser.add_init_script(HOOK).await?;
    let state = ctx.state::<CryptoTraceState>();
    *state
        .persistent_script_id
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(identifier);
    // Also install into the already-loaded page right now.
    let now = ctx.browser.evaluate(HOOK).await?;
    Ok(ok(json!({
        "installed": true,
        "currentPage": now,
        "note": "Hook active for this page and all future loads until crypto_trace_stop. Now trigger the signing action, then call crypto_trace_list.",
    })
    .to_string()))
}

pub struct CryptoTraceList;

impl Tool for CryptoTraceList {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "crypto_trace_list",
            "Return the Web Crypto calls captured since crypto_trace_start — each with operation, algorithm, and previewed key material / message / result (printable bytes shown as text, otherwise hex; truncated). Read this after triggering the signing action.",
            no_arguments(),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, _: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { list(ctx).await.unwrap_or_else(err) })
    }
}

/// `records.length` for whatever the page left in the buffer: the page owns
/// `window.__revCrypto` and may have replaced the array. `Ok(None)` is
/// JavaScript's undefined, which JSON.stringify leaves out.
fn js_length(records: &Value) -> Result<Option<Value>, String> {
    match records {
        Value::Array(items) => Ok(Some(json!(items.len()))),
        Value::String(text) => Ok(Some(json!(utf16_len(text)))),
        Value::Object(fields) => Ok(fields.get("length").cloned()),
        Value::Null => Err("Cannot read properties of null (reading 'length')".into()),
        Value::Bool(_) | Value::Number(_) => Ok(None),
    }
}

async fn list(ctx: &ToolContext) -> Result<ToolResult, String> {
    let raw = ctx
        .browser
        .evaluate("JSON.stringify(window.__revCrypto || [])")
        .await?;
    let raw = raw.as_str().filter(|raw| !raw.is_empty()).unwrap_or("[]");
    let records: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let mut out = Map::new();
    if let Some(count) = js_length(&records)? {
        out.insert("count".into(), count);
    }
    out.insert("records".into(), records);
    Ok(ok_pretty(&out))
}

pub struct CryptoTraceStop;

impl Tool for CryptoTraceStop {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "crypto_trace_stop",
            "Stop Web Crypto tracing: remove the persistent hook and clear the captured buffer.",
            no_arguments(),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, _: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { stop(ctx).await.unwrap_or_else(err) })
    }
}

async fn stop(ctx: &ToolContext) -> Result<ToolResult, String> {
    let state = ctx.state::<CryptoTraceState>();
    let identifier = state
        .persistent_script_id
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    if let Some(identifier) = identifier {
        // A hook that is already gone is as good as removed.
        let _ = ctx.browser.remove_init_script(&identifier).await;
    }
    ctx.browser
        .evaluate("void (delete window.__revCrypto)")
        .await?;
    Ok(ok("crypto trace stopped and buffer cleared"))
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(CryptoTraceStart);
    registry.register(CryptoTraceList);
    registry.register(CryptoTraceStop);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rever_tools_api::tool::{Browser, unsupported};
    use rever_tools_api::utils::Content;

    use super::*;

    /// A page with a `window.__revCrypto` buffer and a list of init scripts.
    #[derive(Default)]
    struct FakeBrowser {
        /// What `JSON.stringify(window.__revCrypto || [])` answers.
        buffer: Mutex<Option<Value>>,
        hook_installed: Mutex<bool>,
        init_scripts: Mutex<Vec<(String, String)>>,
        calls: Mutex<Vec<String>>,
        fail_remove: bool,
        fail_evaluate: bool,
    }

    impl Browser for FakeBrowser {
        fn evaluate<'a>(&'a self, expression: &'a str) -> BoxFuture<'a, Result<Value, String>> {
            Box::pin(async move {
                if self.fail_evaluate {
                    return Err("Execution context was destroyed.".to_owned());
                }
                if expression == HOOK {
                    self.calls.lock().unwrap().push("evaluate HOOK".into());
                    let mut installed = self.hook_installed.lock().unwrap();
                    let answer = if *installed {
                        "already-installed"
                    } else {
                        "installed"
                    };
                    *installed = true;
                    return Ok(json!(answer));
                }
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("evaluate {expression}"));
                match expression {
                    "JSON.stringify(window.__revCrypto || [])" => {
                        Ok(self.buffer.lock().unwrap().clone().unwrap_or(json!("[]")))
                    }
                    "void (delete window.__revCrypto)" => {
                        *self.buffer.lock().unwrap() = None;
                        *self.hook_installed.lock().unwrap() = false;
                        Ok(Value::Null)
                    }
                    other => Err(format!("unexpected expression: {other}")),
                }
            })
        }

        fn add_init_script<'a>(&'a self, source: &'a str) -> BoxFuture<'a, Result<String, String>> {
            Box::pin(async move {
                let mut scripts = self.init_scripts.lock().unwrap();
                let id = format!("init-{}", scripts.len() + 1);
                scripts.push((id.clone(), source.to_owned()));
                self.calls.lock().unwrap().push(format!("add {id}"));
                Ok(id)
            })
        }

        fn remove_init_script<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(format!("remove {id}"));
                if self.fail_remove {
                    return Err("no such script".to_owned());
                }
                self.init_scripts
                    .lock()
                    .unwrap()
                    .retain(|(known, _)| known != id);
                Ok(())
            })
        }
    }

    fn context(browser: FakeBrowser) -> (ToolContext, Arc<FakeBrowser>) {
        let browser = Arc::new(browser);
        let ctx = ToolContext::new(Arc::default(), browser.clone(), std::env::temp_dir());
        (ctx, browser)
    }

    async fn call(tool: impl Tool, ctx: &ToolContext) -> (bool, String) {
        // The tools take no arguments and ignore whatever is sent.
        let result = tool.call(ctx, json!({ "ignored": true })).await;
        let text = match result.content.first() {
            Some(Content::Text { text }) => text.clone(),
            None => String::new(),
        };
        (result.is_error, text)
    }

    fn calls(browser: &FakeBrowser) -> Vec<String> {
        browser.calls.lock().unwrap().clone()
    }

    #[test]
    fn 훅은_ts의_것과_같은_자바스크립트다() {
        assert!(
            HOOK.starts_with("(() => {\n  if (window.__revCrypto) return 'already-installed';")
        );
        assert!(HOOK.ends_with("  return 'installed';\n})();"));
        // String.raw in the TypeScript: the backslash stays a backslash.
        assert!(HOOK.contains(r"split('\n').slice(3, 8)"));
        assert!(HOOK.contains("data.slice(0, 160) + '… (' + data.length + ' chars)'"));
        for name in ["importKey", "sign", "verify", "digest", "encrypt"] {
            assert!(HOOK.contains(&format!("wrap('{name}', ")), "{name}");
        }
        assert_eq!(HOOK.lines().count(), 59);
    }

    #[tokio::test]
    async fn start는_새_문서용_스크립트를_등록하고_현재_페이지에도_훅을_넣는다() {
        let (ctx, browser) = context(FakeBrowser::default());
        assert_eq!(
            call(CryptoTraceStart, &ctx).await,
            (
                false,
                r#"{"installed":true,"currentPage":"installed","note":"Hook active for this page and all future loads until crypto_trace_stop. Now trigger the signing action, then call crypto_trace_list."}"#
                    .to_owned()
            )
        );
        assert_eq!(calls(&browser), ["add init-1", "evaluate HOOK"]);
        assert_eq!(
            *browser.init_scripts.lock().unwrap(),
            [("init-1".to_owned(), HOOK.to_owned())]
        );
        // A second start on the same page finds the hook already there.
        let (_, text) = call(CryptoTraceStart, &ctx).await;
        assert!(
            text.contains(r#""currentPage":"already-installed""#),
            "{text}"
        );
    }

    #[tokio::test]
    async fn 브라우저가_지원하지_않으면_start는_에러를_돌려준다() {
        let ctx = ToolContext::for_tests();
        assert_eq!(
            call(CryptoTraceStart, &ctx).await,
            (true, unsupported("init scripts"))
        );
        assert_eq!(
            call(CryptoTraceList, &ctx).await,
            (true, unsupported("evaluate"))
        );
        assert_eq!(
            call(CryptoTraceStop, &ctx).await,
            (true, unsupported("evaluate"))
        );
    }

    #[tokio::test]
    async fn list는_버퍼의_기록과_개수를_돌려준다() {
        let (ctx, browser) = context(FakeBrowser::default());
        assert_eq!(
            call(CryptoTraceList, &ctx).await,
            (false, "{\n  \"count\": 0,\n  \"records\": []\n}".to_owned())
        );
        *browser.buffer.lock().unwrap() = Some(json!(
            r#"[{"op":"subtle.importKey","stack":["at sign (app.js:1:1)"],"format":"raw","keyMaterial":"secret (6 bytes)","algorithm":"{\"name\":\"HMAC\"}","t":1700000000000}]"#
        ));
        assert_eq!(
            call(CryptoTraceList, &ctx).await,
            (
                false,
                r#"{
  "count": 1,
  "records": [
    {
      "op": "subtle.importKey",
      "stack": [
        "at sign (app.js:1:1)"
      ],
      "format": "raw",
      "keyMaterial": "secret (6 bytes)",
      "algorithm": "{\"name\":\"HMAC\"}",
      "t": 1700000000000
    }
  ]
}"#
                .to_owned()
            )
        );
    }

    #[tokio::test]
    async fn list는_페이지가_바꿔_놓은_버퍼도_자바스크립트처럼_읽는다() {
        let (ctx, browser) = context(FakeBrowser::default());
        let list_with = |raw: Value| {
            let (ctx, browser) = (&ctx, &browser);
            async move {
                *browser.buffer.lock().unwrap() = Some(raw);
                call(CryptoTraceList, ctx).await
            }
        };
        // Not a string, or an empty one: `raw || '[]'`.
        for raw in [Value::Null, json!(""), json!(0)] {
            assert_eq!(
                list_with(raw).await.1,
                "{\n  \"count\": 0,\n  \"records\": []\n}"
            );
        }
        assert_eq!(
            list_with(json!("{\"a\":1}")).await.1,
            "{\n  \"records\": {\n    \"a\": 1\n  }\n}"
        );
        assert_eq!(
            list_with(json!("{\"length\":\"x\"}")).await.1,
            "{\n  \"count\": \"x\",\n  \"records\": {\n    \"length\": \"x\"\n  }\n}"
        );
        assert_eq!(
            list_with(json!("\"한😀\"")).await.1,
            "{\n  \"count\": 3,\n  \"records\": \"한😀\"\n}"
        );
        assert_eq!(list_with(json!("7")).await.1, "{\n  \"records\": 7\n}");
        assert_eq!(
            list_with(json!("null")).await,
            (
                true,
                "Cannot read properties of null (reading 'length')".to_owned()
            )
        );
        let (is_error, text) = list_with(json!("[oops")).await;
        assert!(is_error);
        assert!(!text.is_empty());
    }

    #[tokio::test]
    async fn stop은_등록한_스크립트를_지우고_버퍼를_비운다() {
        let (ctx, browser) = context(FakeBrowser::default());
        call(CryptoTraceStart, &ctx).await;
        browser.calls.lock().unwrap().clear();
        assert_eq!(
            call(CryptoTraceStop, &ctx).await,
            (false, "crypto trace stopped and buffer cleared".to_owned())
        );
        assert_eq!(
            calls(&browser),
            ["remove init-1", "evaluate void (delete window.__revCrypto)"]
        );
        assert!(browser.init_scripts.lock().unwrap().is_empty());
        // Nothing is registered any more: a second stop only clears the buffer.
        browser.calls.lock().unwrap().clear();
        assert!(!call(CryptoTraceStop, &ctx).await.0);
        assert_eq!(
            calls(&browser),
            ["evaluate void (delete window.__revCrypto)"]
        );
    }

    #[tokio::test]
    async fn stop은_스크립트_제거가_실패해도_버퍼를_비운다() {
        let (ctx, browser) = context(FakeBrowser {
            fail_remove: true,
            ..FakeBrowser::default()
        });
        call(CryptoTraceStart, &ctx).await;
        assert_eq!(
            call(CryptoTraceStop, &ctx).await,
            (false, "crypto trace stopped and buffer cleared".to_owned())
        );
        // The id is forgotten either way, so it is not removed twice.
        browser.calls.lock().unwrap().clear();
        call(CryptoTraceStop, &ctx).await;
        assert_eq!(
            calls(&browser),
            ["evaluate void (delete window.__revCrypto)"]
        );
    }

    #[tokio::test]
    async fn 페이지_평가가_실패하면_그_메시지를_돌려준다() {
        let (ctx, browser) = context(FakeBrowser {
            fail_evaluate: true,
            ..FakeBrowser::default()
        });
        let expected = (true, "Execution context was destroyed.".to_owned());
        // The init script is registered before the page is touched, as in the TypeScript.
        assert_eq!(call(CryptoTraceStart, &ctx).await, expected);
        assert_eq!(browser.init_scripts.lock().unwrap().len(), 1);
        assert_eq!(call(CryptoTraceList, &ctx).await, expected);
        assert_eq!(call(CryptoTraceStop, &ctx).await, expected);
    }
}
