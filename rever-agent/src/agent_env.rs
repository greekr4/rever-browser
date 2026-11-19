//! Port of `src/main/agent-env.ts` — the environment an agent child process starts with.

use std::collections::{BTreeMap, HashSet};

use crate::acp_detect::{extra_dirs, node_path, path_delimiter};
use crate::claude_cli::ClaudeCliFinder;

/// Environment for a child process, keyed by the exact variable name.
pub type AgentEnv = BTreeMap<String, String>;

/// Builds the child environment from `base`.
///
/// The Claude Code CLI sees CLAUDECODE / CLAUDE_CODE_* inherited from its parent
/// and refuses to start, taking itself for a nested session (this happens when
/// the app itself is launched from inside a Claude Code session). An agent child
/// must be an independent session, so those variables are removed.
///
/// Names are matched exactly, as in the TypeScript, where the copy of
/// `process.env` is a plain case-sensitive object. On Windows that means a
/// variable spelled `Path` is not seen as `PATH`: the result then carries both
/// `Path` (inherited) and `PATH` (only the prepended directories).
pub fn agent_env_from(
    command: &str,
    base: impl IntoIterator<Item = (String, String)>,
    windows: bool,
    extra_dirs: &[String],
    find_claude_cli: impl FnOnce() -> Option<String>,
) -> AgentEnv {
    let mut env: AgentEnv = base.into_iter().collect();
    // CLAUDE_CODE_EXECUTABLE is not a session marker — it tells claude-agent-acp
    // which Claude Code binary to spawn, and must survive the scrub below.
    let is_claude_session_var = |name: &str| {
        name != "CLAUDE_CODE_EXECUTABLE"
            && (name == "CLAUDECODE" || name.starts_with("CLAUDE_CODE_"))
    };
    // Windows environment variable names are case-insensitive, so a variant
    // spelling such as 'ClaudeCode' can be inherited too. Compare in upper case.
    env.retain(|key, _| {
        if windows {
            !is_claude_session_var(&key.to_uppercase())
        } else {
            !is_claude_session_var(key)
        }
    });

    // Without this, claude-agent-acp runs the Claude Code CLI bundled in its SDK
    // dependency (2.1.83 as of acp 0.23.x), which the API refuses for Claude 5
    // models ("version 2.1.280 or newer is required") and whose `default`
    // alias is still Opus 4.6. Prefer the user's installed CLI; an explicit
    // CLAUDE_CODE_EXECUTABLE in the environment always wins.
    if env
        .get("CLAUDE_CODE_EXECUTABLE")
        .is_none_or(String::is_empty)
        && let Some(cli) = find_claude_cli()
    {
        env.insert("CLAUDE_CODE_EXECUTABLE".to_owned(), cli);
    }

    // If a claude-agent-acp session settles on a Claude 5 default model the API
    // rejects the request, so Claude agents get their default pinned to opus-4-8.
    // Switching in the session's model picker outranks the env and still works.
    if command.contains("claude") {
        env.insert("ANTHROPIC_MODEL".to_owned(), "claude-opus-4-8".to_owned());
    }

    // A packaged app launched from Finder does not inherit the login shell's
    // PATH and is left with `/usr/bin:/bin`. The agent binary (passed as an
    // absolute path) then fails at its `#!/usr/bin/env node` shebang and the
    // spawn dies at once. node usually sits in the same bin/ as the agent binary
    // (nvm/volta), so that directory and the common install locations go in
    // front of PATH and the shebang always resolves.
    let mut prepend: Vec<String> = Vec::new();
    if node_path::is_absolute(command, windows) {
        prepend.push(node_path::dirname(command, windows));
    }
    prepend.extend_from_slice(extra_dirs);
    let delimiter = path_delimiter(windows);
    let existing = env.get("PATH").cloned().unwrap_or_default();
    let mut seen = HashSet::new();
    let path: Vec<&str> = prepend
        .iter()
        .map(String::as_str)
        .chain(existing.split(delimiter))
        .filter(|dir| !dir.is_empty() && seen.insert(*dir))
        .collect();
    env.insert("PATH".to_owned(), path.join(&delimiter.to_string()));
    env
}

/// [`agent_env_from`] for this process's environment.
pub fn agent_env(command: &str, claude_cli: &ClaudeCliFinder) -> AgentEnv {
    // Node decodes the environment leniently; `std::env::vars` would panic on
    // a value that is not Unicode.
    let base = std::env::vars_os().map(|(key, value)| {
        (
            key.to_string_lossy().into_owned(),
            value.to_string_lossy().into_owned(),
        )
    });
    agent_env_from(command, base, cfg!(windows), &extra_dirs(), || {
        claude_cli.find().map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inherited environment each case starts from: like the TypeScript
    /// `beforeEach`, it has no ANTHROPIC_MODEL and no session markers.
    fn base(extra: &[(&str, &str)]) -> Vec<(String, String)> {
        [("HOME", "/home/u"), ("PATH", "/usr/bin:/bin")]
            .iter()
            .chain(extra)
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn env_for(command: &str, extra: &[(&str, &str)]) -> AgentEnv {
        agent_env_from(command, base(extra), false, &[], || None)
    }

    fn get<'a>(env: &'a AgentEnv, name: &str) -> Option<&'a str> {
        env.get(name).map(String::as_str)
    }

    // ── agentEnv ──

    #[test]
    fn claude_계열_에이전트는_기본_모델을_claude_opus_4_8로_고정한다() {
        let env = env_for("claude-agent-acp", &[]);
        assert_eq!(get(&env, "ANTHROPIC_MODEL"), Some("claude-opus-4-8"));
    }

    #[test]
    fn claude_계열이_아니면_모델을_고정하지_않는다() {
        let env = env_for("/usr/local/bin/codex-acp", &[]);
        assert_eq!(get(&env, "ANTHROPIC_MODEL"), None);
    }

    #[test]
    fn claudecode와_claude_code_세션_마커는_제거한다() {
        let env = env_for(
            "claude-agent-acp",
            &[("CLAUDECODE", "1"), ("CLAUDE_CODE_SOMETHING", "1")],
        );
        assert_eq!(get(&env, "CLAUDECODE"), None);
        assert_eq!(get(&env, "CLAUDE_CODE_SOMETHING"), None);
    }

    // ── not covered by the TypeScript tests ──

    #[test]
    fn 실제_프로세스_환경에서도_같은_규칙이_적용된다() {
        let finder = ClaudeCliFinder::new();
        let env = agent_env("claude-agent-acp", &finder);
        assert_eq!(get(&env, "ANTHROPIC_MODEL"), Some("claude-opus-4-8"));
        assert_eq!(get(&env, "CLAUDECODE"), None);
        assert!(get(&env, "PATH").is_some_and(|path| !path.is_empty()));
    }

    #[test]
    fn 상속된_모델_설정은_claude가_아닌_에이전트에_그대로_남는다() {
        let env = env_for("codex-acp", &[("ANTHROPIC_MODEL", "claude-x")]);
        assert_eq!(get(&env, "ANTHROPIC_MODEL"), Some("claude-x"));
        let env = env_for("/opt/claude/bin/agent", &[("ANTHROPIC_MODEL", "claude-x")]);
        assert_eq!(get(&env, "ANTHROPIC_MODEL"), Some("claude-opus-4-8"));
    }

    #[test]
    fn claude_code_executable은_세션_마커가_아니라서_남긴다() {
        let env = agent_env_from(
            "codex-acp",
            base(&[("CLAUDE_CODE_EXECUTABLE", "/mine/claude")]),
            false,
            &[],
            || Some("/found/claude".to_owned()),
        );
        assert_eq!(get(&env, "CLAUDE_CODE_EXECUTABLE"), Some("/mine/claude"));
    }

    #[test]
    fn claude_code_executable이_없거나_비었으면_찾은_cli를_넣는다() {
        let found = || Some("/found/claude".to_owned());
        let env = agent_env_from("codex-acp", base(&[]), false, &[], found);
        assert_eq!(get(&env, "CLAUDE_CODE_EXECUTABLE"), Some("/found/claude"));

        let empty = base(&[("CLAUDE_CODE_EXECUTABLE", "")]);
        let env = agent_env_from("codex-acp", empty, false, &[], found);
        assert_eq!(get(&env, "CLAUDE_CODE_EXECUTABLE"), Some("/found/claude"));

        let env = env_for("codex-acp", &[]);
        assert_eq!(get(&env, "CLAUDE_CODE_EXECUTABLE"), None);
    }

    #[test]
    fn posix에서는_대소문자가_다른_이름을_세션_마커로_보지_않는다() {
        let env = env_for("codex-acp", &[("ClaudeCode", "1"), ("claude_code_x", "1")]);
        assert_eq!(get(&env, "ClaudeCode"), Some("1"));
        assert_eq!(get(&env, "claude_code_x"), Some("1"));
    }

    #[test]
    fn windows에서는_대소문자_변형_세션_마커도_제거한다() {
        let inherited = base(&[
            ("ClaudeCode", "1"),
            ("Claude_Code_Entrypoint", "cli"),
            ("Claude_Code_Executable", "C:\\claude.exe"),
        ]);
        let env = agent_env_from("codex-acp", inherited, true, &[], || None);
        assert_eq!(get(&env, "ClaudeCode"), None);
        assert_eq!(get(&env, "Claude_Code_Entrypoint"), None);
        assert_eq!(get(&env, "Claude_Code_Executable"), Some("C:\\claude.exe"));
    }

    #[test]
    fn 에이전트_디렉터리와_추가_디렉터리를_path_앞에_중복_없이_붙인다() {
        let extra = ["/opt/homebrew/bin".to_owned(), "/usr/bin".to_owned()];
        let inherited = base(&[("PATH", "/usr/bin::/bin:/nvm/bin:/bin")]);
        let env = agent_env_from("/nvm/bin/codex-acp", inherited, false, &extra, || None);
        assert_eq!(
            get(&env, "PATH"),
            Some("/nvm/bin:/opt/homebrew/bin:/usr/bin:/bin")
        );
    }

    #[test]
    fn 상대_경로_명령은_path에_디렉터리를_더하지_않는다() {
        let extra = ["/usr/local/bin".to_owned()];
        let env = agent_env_from("bin/codex-acp", base(&[]), false, &extra, || None);
        assert_eq!(get(&env, "PATH"), Some("/usr/local/bin:/usr/bin:/bin"));
        let env = agent_env_from("", base(&[]), false, &[], || None);
        assert_eq!(get(&env, "PATH"), Some("/usr/bin:/bin"));
    }

    #[test]
    fn path가_없어도_추가_디렉터리만으로_path를_만든다() {
        let extra = ["/usr/local/bin".to_owned()];
        let env = agent_env_from("/a/agent", Vec::new(), false, &extra, || None);
        assert_eq!(get(&env, "PATH"), Some("/a:/usr/local/bin"));
    }

    #[test]
    fn windows_path는_세미콜론으로_잇고_이름이_다른_path는_건드리지_않는다() {
        let extra = ["C:\\Program Files\\nodejs".to_owned()];
        let inherited = vec![("Path".to_owned(), "C:\\Windows".to_owned())];
        let env = agent_env_from("C:\\npm\\codex-acp.cmd", inherited, true, &extra, || None);
        // Same as the TypeScript: `env.PATH` does not see the `Path` spelling.
        assert_eq!(get(&env, "PATH"), Some("C:\\npm;C:\\Program Files\\nodejs"));
        assert_eq!(get(&env, "Path"), Some("C:\\Windows"));

        let inherited = vec![("PATH".to_owned(), "C:\\Windows;C:\\npm".to_owned())];
        let env = agent_env_from("C:\\npm\\codex-acp.cmd", inherited, true, &extra, || None);
        assert_eq!(
            get(&env, "PATH"),
            Some("C:\\npm;C:\\Program Files\\nodejs;C:\\Windows")
        );
    }
}
