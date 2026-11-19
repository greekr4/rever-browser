//! Port of `src/main/claude-cli.ts` — locate the user's installed Claude Code CLI.
//!
//! The user's own CLI is located so that claude-agent-acp drives it
//! instead of the CLI bundled inside its @anthropic-ai/claude-agent-sdk
//! dependency (0.2.83 → Claude Code 2.1.83). The bundled copy predates the
//! Claude 5 models: the API rejects them with "Claude Code 2.1.83 does not
//! support this model; version 2.1.280 or newer is required", and its
//! `default` alias still resolves to Opus 4.6. claude-agent-acp honours
//! CLAUDE_CODE_EXECUTABLE for the path it spawns, so we point it at the real CLI.

use std::sync::OnceLock;

use crate::acp_detect::{env_lossy, extra_dirs, is_executable, node_path, path_delimiter};

/// Pure resolver: first `claude` candidate in `dirs` for which `is_executable`
/// holds. Windows installs are `claude.exe` / `claude.cmd`.
pub fn pick_claude_cli(
    dirs: &[impl AsRef<str>],
    is_executable: impl Fn(&str) -> bool,
    windows: bool,
) -> Option<String> {
    let names: &[&str] = if windows {
        &["claude.exe", "claude.cmd", "claude"]
    } else {
        &["claude"]
    };
    for dir in dirs.iter().map(AsRef::as_ref).filter(|dir| !dir.is_empty()) {
        for name in names {
            let candidate = node_path::join(&[dir, name], windows);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// [`pick_claude_cli`] over PATH followed by `extra_dirs`.
pub fn find_claude_cli_in(
    path_env: Option<&str>,
    extra_dirs: &[String],
    is_executable: impl Fn(&str) -> bool,
    windows: bool,
) -> Option<String> {
    let dirs: Vec<&str> = path_env
        .unwrap_or_default()
        .split(path_delimiter(windows))
        .chain(extra_dirs.iter().map(String::as_str))
        .collect();
    pick_claude_cli(&dirs, is_executable, windows)
}

/// Resolves the user's `claude` binary once (PATH + common dirs) and remembers
/// the answer, including "not installed". The app keeps one for its lifetime.
#[derive(Debug, Default)]
pub struct ClaudeCliFinder {
    cached: OnceLock<Option<String>>,
}

impl ClaudeCliFinder {
    pub const fn new() -> Self {
        Self {
            cached: OnceLock::new(),
        }
    }

    pub fn find(&self) -> Option<&str> {
        self.find_with(|| {
            let path = env_lossy("PATH");
            find_claude_cli_in(path.as_deref(), &extra_dirs(), is_executable, cfg!(windows))
        })
    }

    fn find_with(&self, resolve: impl FnOnce() -> Option<String>) -> Option<&str> {
        self.cached.get_or_init(resolve).as_deref()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    // ── pickClaudeCli ──

    #[test]
    fn 디렉터리_순서대로_첫_번째_실행_가능한_claude를_고른다() {
        let exists = |p: &str| p == "/home/u/.local/bin/claude" || p == "/usr/local/bin/claude";
        assert_eq!(
            pick_claude_cli(
                &["/opt/homebrew/bin", "/home/u/.local/bin", "/usr/local/bin"],
                exists,
                false
            )
            .as_deref(),
            Some("/home/u/.local/bin/claude")
        );
    }

    #[test]
    fn 없으면_none을_돌려준다() {
        assert_eq!(pick_claude_cli(&["/a", "/b"], |_| false, false), None);
    }

    #[test]
    fn 빈_디렉터리_항목은_건너뛴다() {
        let seen = RefCell::new(Vec::new());
        pick_claude_cli(
            &["", "/a"],
            |p| {
                seen.borrow_mut().push(p.to_owned());
                false
            },
            false,
        );
        assert_eq!(*seen.borrow(), ["/a/claude"]);
    }

    #[test]
    fn windows에서는_exe_cmd_변형도_찾는다() {
        let exists = |p: &str| p.ends_with("claude.cmd");
        let found = pick_claude_cli(&["C:\\npm"], exists, true);
        assert!(found.is_some_and(|path| path.ends_with("claude.cmd")));
    }

    // ── not covered by the TypeScript tests ──

    #[test]
    fn windows에서는_exe_cmd_확장자_없음_순서로_본다() {
        let seen = RefCell::new(Vec::new());
        let found = pick_claude_cli(
            &["C:\\npm"],
            |p| {
                seen.borrow_mut().push(p.to_owned());
                p.ends_with("\\claude")
            },
            true,
        );
        assert_eq!(found.as_deref(), Some("C:\\npm\\claude"));
        assert_eq!(
            *seen.borrow(),
            [
                "C:\\npm\\claude.exe",
                "C:\\npm\\claude.cmd",
                "C:\\npm\\claude"
            ]
        );
    }

    #[test]
    fn path를_먼저_보고_추가_디렉터리를_나중에_본다() {
        let extra = ["/usr/local/bin".to_owned()];
        let everywhere = |_: &str| true;
        assert_eq!(
            find_claude_cli_in(Some(":/opt/x/bin:/usr/bin"), &extra, everywhere, false).as_deref(),
            Some("/opt/x/bin/claude")
        );
        let only_extra = |p: &str| p == "/usr/local/bin/claude";
        assert_eq!(
            find_claude_cli_in(Some("/usr/bin"), &extra, only_extra, false).as_deref(),
            Some("/usr/local/bin/claude")
        );
        assert_eq!(
            find_claude_cli_in(None, &extra, only_extra, false).as_deref(),
            Some("/usr/local/bin/claude")
        );
        assert_eq!(
            find_claude_cli_in(Some("C:\\a;C:\\b"), &[], |p| p == "C:\\b\\claude.exe", true)
                .as_deref(),
            Some("C:\\b\\claude.exe")
        );
    }

    #[test]
    fn 한_번_찾은_결과를_다시_찾지_않는다() {
        let calls = Cell::new(0);
        let resolve = || {
            calls.set(calls.get() + 1);
            Some("/bin/claude".to_owned())
        };
        let finder = ClaudeCliFinder::new();
        assert_eq!(finder.find_with(resolve), Some("/bin/claude"));
        assert_eq!(finder.find_with(resolve), Some("/bin/claude"));
        assert_eq!(finder.find(), Some("/bin/claude"));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn 못_찾은_결과도_기억한다() {
        let calls = Cell::new(0);
        let resolve = || {
            calls.set(calls.get() + 1);
            None
        };
        let finder = ClaudeCliFinder::new();
        assert_eq!(finder.find_with(resolve), None);
        assert_eq!(finder.find_with(resolve), None);
        assert_eq!(calls.get(), 1);
    }
}
