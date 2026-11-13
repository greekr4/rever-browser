//! Port of `src/main/mcp/tools/override.ts` — local overrides: answer a request from
//! a given body or file instead of the network.
//!
//! The TypeScript served an override through the page layer's request interception.
//! `Browser` cannot intercept requests, so `override_add` checks its arguments and
//! the file as before and then reports the registration as unsupported. No override
//! can therefore exist, which is exactly what `override_list` and `override_remove`
//! answer; the TypeScript's set of override ids would always be empty and is not kept.

use std::path::{Component, Path, PathBuf};
use std::{fs, io};

use rever_tools_api::tool::{ToolDef, ToolRegistry, unsupported};
use serde_json::json;

use crate::{no_args, schema};

const OVERRIDE_FILE_SIZE_LIMIT: u64 = 5 * 1024 * 1024; // 5MB

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
        }
        Err(unsupported("response overrides"))
    }
);

tool!(
    OverrideList,
    ToolDef::new("override_list", "List active local overrides.", no_args()),
    |_ctx, _args| { Ok("[]".to_owned()) }
);

tool!(
    OverrideRemove,
    ToolDef::new(
        "override_remove",
        "Remove a local override by ID.",
        schema(json!({ "id": { "type": "string" } }), &["id"])
    ),
    |_ctx, args| { Err(format!("not an override: {}", args.str("id"))) }
);

#[cfg(test)]
mod tests {
    use serde_json::json;

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
            Err(unsupported("response overrides"))
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
                Err(unsupported("response overrides"))
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
}
