//! Port of `src/main/mcp/tools/override.ts` — local overrides: answer a request from
//! a given body or file instead of the network.
//!
//! An override is an intercept rule that answers at the request stage with a
//! body, kept in the list the intercept tools use, as in the TypeScript; the ids
//! of the rules that are overrides are remembered beside it.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::{fs, io};

use std::sync::Mutex;

use rever_tools_api::tool::{ToolDef, ToolRegistry};
use rever_tools_debug::intercept::{InterceptRule, InterceptState, Mode, Stage};
use serde_json::{Value, json};

use crate::{no_args, schema};

const OVERRIDE_FILE_SIZE_LIMIT: u64 = 5 * 1024 * 1024; // 5MB

/// The ids of the intercept rules that are overrides.
#[derive(Default)]
pub struct Overrides(Mutex<HashSet<String>>);

impl Overrides {
    fn ids(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(OverrideAdd);
    registry.register(OverrideList);
    registry.register(OverrideRemove);
}

/// Node's `path.resolve`: absolute, with `.` and `..` folded away without touching
/// the file system.
fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut resolved = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            // Stays at the root when there is nothing left to leave.
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

/// The file's path if it lies in the only folder overrides may be read from,
/// `<data folder>/overrides`; otherwise the error to report.
fn resolve_override_path(data_dir: &Path, file: &str) -> Result<PathBuf, String> {
    let allowed_dir = resolve(&data_dir.join("overrides"));
    let resolved = resolve(Path::new(file));
    // Compares whole path components, so "overrides-evil" is not inside "overrides".
    if !resolved.starts_with(&allowed_dir) {
        return Err(format!(
            "file path is outside the allowed override directory ({})",
            allowed_dir.display()
        ));
    }
    Ok(resolved)
}

/// The failure in the words of Node's `statSync`, which is what the agent was shown.
fn stat_error(error: &io::Error, path: &Path) -> String {
    let what = match error.kind() {
        io::ErrorKind::NotFound => "ENOENT: no such file or directory",
        io::ErrorKind::PermissionDenied => "EACCES: permission denied",
        io::ErrorKind::NotADirectory => "ENOTDIR: not a directory",
        // shortcut: rarer failures keep the wording of the Rust standard library.
        _ => return error.to_string(),
    };
    format!("{what}, stat '{}'", path.display())
}

tool!(
    OverrideAdd,
    ToolDef::new(
        "override_add",
        "Register a local override: when a URL matching `urlPattern` is requested, fulfill it with `body` (or contents of `file`) instead of hitting the network. Equivalent to DevTools \"Local Overrides\". Use to neutralize anti-bot scripts, replace a bundle with a beautified copy, or patch a comparison.",
        schema(
            json!({
                "urlPattern": {
                    "type": "string",
                    "description": "CDP wildcard pattern, e.g. \"*/static/main.js\""
                },
                "body": { "description": "Inline replacement body", "type": "string" },
                "file": {
                    "description": "Local file path to load as the body (mutually exclusive with body)",
                    "type": "string"
                },
                "mimeType": { "type": "string" }
            }),
            &["urlPattern"]
        )
    ),
    |ctx, args| {
        let body = args.opt_str("body");
        // An empty path counted as "no file" in the TypeScript.
        let file = args.opt_str("file").filter(|file| !file.is_empty());
        if body.is_none() && file.is_none() {
            return Err("provide body or file".to_owned());
        }
        let mut text = body.unwrap_or_default().to_owned();
        if let Some(file) = file {
            let resolved = resolve_override_path(&ctx.data_dir, file)?;
            let stat = fs::metadata(&resolved)
                .map_err(|error| format!("cannot stat file: {}", stat_error(&error, &resolved)))?;
            if stat.len() > OVERRIDE_FILE_SIZE_LIMIT {
                return Err(format!(
                    "file exceeds size limit ({OVERRIDE_FILE_SIZE_LIMIT} bytes)"
                ));
            }
            // What reading a folder (the allowed one included) failed with.
            if stat.is_dir() {
                return Err("EISDIR: illegal operation on a directory, read".to_owned());
            }
            // `readFileSync(path, 'utf8')`: bytes that are not UTF-8 become U+FFFD.
            let bytes = fs::read(&resolved).map_err(|error| stat_error(&error, &resolved))?;
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
        let url_pattern = args.str("urlPattern");
        let mime_type = args.opt_str("mimeType").filter(|mime| !mime.is_empty());
        let rule = InterceptRule {
            id: uuid::Uuid::new_v4().to_string(),
            url_pattern: url_pattern.to_owned(),
            mode: Mode::Modify,
            stage: Stage::Request,
            modify_headers: mime_type
                .map(|mime| vec![("content-type".to_owned(), mime.to_owned())]),
            replace_body: Some(text.clone()),
        };
        let id = rule.id.clone();
        ctx.state::<InterceptState>().add_rule(rule).await?;
        ctx.state::<Overrides>().ids().insert(id.clone());
        Ok(crate::pretty(&json!({
            "id": id,
            "urlPattern": url_pattern,
            "bytes": text.encode_utf16().count(),
        })))
    }
);

tool!(
    OverrideList,
    ToolDef::new("override_list", "List active local overrides.", no_args()),
    |ctx, _args| {
        let ids = ctx.state::<Overrides>().ids().clone();
        let rules = ctx.state::<InterceptState>().rules();
        let listed: Vec<Value> = rules
            .iter()
            .filter(|rule| ids.contains(&rule.id))
            .map(|rule| {
                let body = rule.replace_body.as_deref().unwrap_or_default();
                let mut entry = json!({
                    "id": rule.id,
                    "urlPattern": rule.url_pattern,
                    "bytes": body.encode_utf16().count(),
                });
                let headers = rule.modify_headers.iter().flatten();
                if let Some((_, mime)) =
                    headers.into_iter().find(|(name, _)| name == "content-type")
                {
                    entry["mimeType"] = json!(mime);
                }
                entry
            })
            .collect();
        Ok(crate::pretty(&json!(listed)))
    }
);

tool!(
    OverrideRemove,
    ToolDef::new(
        "override_remove",
        "Remove a local override by ID.",
        schema(json!({ "id": { "type": "string" } }), &["id"])
    ),
    |ctx, args| {
        let id = args.str("id");
        if !ctx.state::<Overrides>().ids().remove(id) {
            return Err(format!("not an override: {id}"));
        }
        ctx.state::<InterceptState>().remove_rule(id).await;
        Ok(format!("removed {id}"))
    }
);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use rever_tools_api::tool::unsupported;

    use super::*;
    use crate::test_support::{FakeBrowser, call, context, invalid};

    fn outside(ctx: &rever_tools_api::tool::ToolContext) -> Result<String, String> {
        Err(format!(
            "file path is outside the allowed override directory ({})",
            ctx.data_dir.join("overrides").display()
        ))
    }

    #[test]
    fn 경로는_파일_시스템을_보지_않고_정리한다() {
        assert_eq!(resolve(Path::new("/a/b/../c/./d")), PathBuf::from("/a/c/d"));
        assert_eq!(resolve(Path::new("/../..")), PathBuf::from("/"));
        assert_eq!(resolve(Path::new("/a/b/")), PathBuf::from("/a/b"));
        assert_eq!(
            resolve(Path::new("x/../y")),
            std::env::current_dir().unwrap().join("y")
        );
    }

    #[tokio::test]
    async fn 본문도_파일도_없으면_거절한다() {
        let (_, ctx) = context(FakeBrowser::default());
        for args in [
            json!({ "urlPattern": "*/main.js" }),
            json!({ "urlPattern": "*/main.js", "file": "" }),
        ] {
            assert_eq!(
                call(&OverrideAdd, &ctx, args).await,
                Err("provide body or file".to_owned())
            );
        }
    }

    #[tokio::test]
    async fn 잘못된_인자를_거절한다() {
        let (_, ctx) = context(FakeBrowser::default());
        for (args, key, received) in [
            (json!({ "body": "x" }), "urlPattern", "undefined"),
            (json!({ "urlPattern": "*", "body": 1 }), "body", "number"),
            (json!({ "urlPattern": "*", "file": null }), "file", "null"),
            (
                json!({ "urlPattern": "*", "body": "x", "mimeType": [] }),
                "mimeType",
                "array",
            ),
        ] {
            assert_eq!(
                call(&OverrideAdd, &ctx, args).await,
                invalid("override_add", key, "string", received)
            );
        }
        assert_eq!(
            call(&OverrideRemove, &ctx, json!({})).await,
            invalid("override_remove", "id", "string", "undefined")
        );
    }

    #[tokio::test]
    async fn 허용_폴더_밖의_파일은_읽지_않는다() {
        let (_, ctx) = context(FakeBrowser::default());
        let dir = ctx.data_dir.join("overrides");
        for file in [
            "/etc/passwd".to_owned(),
            "relative.js".to_owned(),
            // Leaves the folder through "..".
            dir.join("../secret.js").display().to_string(),
            dir.join("a/../../injected-snippets.json")
                .display()
                .to_string(),
            // A sibling whose name merely starts the same.
            format!("{}-evil/x.js", dir.display()),
        ] {
            assert_eq!(
                call(
                    &OverrideAdd,
                    &ctx,
                    json!({ "urlPattern": "*", "file": file })
                )
                .await,
                outside(&ctx),
                "{file}"
            );
        }
        // The file wins over the body, as in the TypeScript, so its check still applies.
        let args = json!({ "urlPattern": "*", "body": "x", "file": "/etc/passwd" });
        assert_eq!(call(&OverrideAdd, &ctx, args).await, outside(&ctx));
    }

    #[tokio::test]
    async fn 없는_파일과_너무_큰_파일을_거절한다() {
        let (_, ctx) = context(FakeBrowser::default());
        let dir = ctx.data_dir.join("overrides");
        fs::create_dir_all(&dir).unwrap();

        // The path is printed resolved, in the words of Node's statSync.
        let missing = dir.join("sub/../missing.js").display().to_string();
        assert_eq!(
            call(
                &OverrideAdd,
                &ctx,
                json!({ "urlPattern": "*", "file": missing }),
            )
            .await,
            Err(format!(
                "cannot stat file: ENOENT: no such file or directory, stat '{}'",
                dir.join("missing.js").display()
            ))
        );
        // A folder passes the stat and then cannot be read.
        for folder in [dir.clone(), dir.join("sub/..")] {
            assert_eq!(
                call(
                    &OverrideAdd,
                    &ctx,
                    json!({ "urlPattern": "*", "file": folder.display().to_string() }),
                )
                .await,
                Err("EISDIR: illegal operation on a directory, read".to_owned())
            );
        }
        fs::write(dir.join("plain"), "x").unwrap();
        let below_a_file = dir.join("plain/x.js");
        assert_eq!(
            call(
                &OverrideAdd,
                &ctx,
                json!({ "urlPattern": "*", "file": below_a_file.display().to_string() }),
            )
            .await,
            Err(format!(
                "cannot stat file: ENOTDIR: not a directory, stat '{}'",
                below_a_file.display()
            ))
        );

        // One byte over the limit; exactly at the limit is accepted.
        let big = dir.join("big.js");
        let file = fs::File::create(&big).unwrap();
        file.set_len(5 * 1024 * 1024 + 1).unwrap();
        let args = json!({ "urlPattern": "*", "file": big.display().to_string() });
        assert_eq!(
            call(&OverrideAdd, &ctx, args.clone()).await,
            Err("file exceeds size limit (5242880 bytes)".to_owned())
        );
        file.set_len(5 * 1024 * 1024).unwrap();
        assert_eq!(
            call(&OverrideAdd, &ctx, args).await,
            Err(unsupported("request interception"))
        );
        let _ = fs::remove_dir_all(&ctx.data_dir);
    }

    #[tokio::test]
    async fn 검사를_통과한_오버라이드도_등록은_지원하지_않는다고_알린다() {
        let (browser, ctx) = context(FakeBrowser::default());
        let dir = ctx.data_dir.join("overrides");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/main.js"), "console.log(1)").unwrap();
        for args in [
            json!({ "urlPattern": "*/main.js", "body": "" }),
            json!({ "urlPattern": "*/main.js", "body": "x", "mimeType": "text/javascript" }),
            json!({ "urlPattern": "*/main.js", "file": dir.join("sub/./main.js").display().to_string() }),
        ] {
            assert_eq!(
                call(&OverrideAdd, &ctx, args).await,
                Err(unsupported("request interception"))
            );
        }
        assert!(browser.calls().is_empty());
        let _ = fs::remove_dir_all(&ctx.data_dir);
    }

    #[tokio::test]
    async fn 등록된_오버라이드가_없으므로_목록은_비고_제거는_모르는_id다() {
        let (_, ctx) = context(FakeBrowser::default());
        assert_eq!(
            call(&OverrideList, &ctx, json!({})).await,
            Ok("[]".to_owned())
        );
        assert_eq!(
            call(&OverrideRemove, &ctx, json!({ "id": "abc" })).await,
            Err("not an override: abc".to_owned())
        );
    }

    /// An engine that intercepts whatever it is given.
    struct Accepting;

    impl rever_tools_debug::intercept::InterceptEngine for Accepting {
        fn apply_rules<'a>(
            &'a self,
            _: &'a [InterceptRule],
        ) -> rever_tools_api::tool::BoxFuture<'a, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn 오버라이드를_등록하고_나열하고_지운다() {
        let (_, ctx) = context(FakeBrowser::default());
        let state = ctx.state::<InterceptState>();
        state.set_engine(std::sync::Arc::new(Accepting));
        let args =
            json!({ "urlPattern": "*/main.js", "body": "한글 ok", "mimeType": "text/javascript" });
        let added: Value =
            serde_json::from_str(&call(&OverrideAdd, &ctx, args).await.unwrap()).unwrap();
        // Bytes are UTF-16 units, as `text.length` counted them.
        assert_eq!(
            (added["urlPattern"].as_str(), added["bytes"].as_u64()),
            (Some("*/main.js"), Some(5))
        );
        let id = added["id"].as_str().unwrap();

        let rule = state
            .matching_rule("https://a.test/static/main.js")
            .unwrap();
        assert_eq!((rule.mode, rule.stage), (Mode::Modify, Stage::Request));
        assert_eq!(rule.replace_body.as_deref(), Some("한글 ok"));

        let listed = call(&OverrideList, &ctx, json!({})).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&listed).unwrap(),
            json!([{ "id": id, "urlPattern": "*/main.js", "bytes": 5, "mimeType": "text/javascript" }])
        );
        assert_eq!(
            call(&OverrideRemove, &ctx, json!({ "id": id })).await,
            Ok(format!("removed {id}"))
        );
        assert!(state.rules().is_empty());
        assert_eq!(
            call(&OverrideRemove, &ctx, json!({ "id": id })).await,
            Err(format!("not an override: {id}"))
        );
    }
}
