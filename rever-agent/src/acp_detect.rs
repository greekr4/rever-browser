//! Port of `src/main/acp-detect.ts` — find agent CLIs on PATH and in the usual install dirs.
//!
//! The TypeScript is async; these functions block instead (the crate has no
//! runtime), so callers run them off the UI thread.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProbe {
    /// Primary binary to look for.
    pub command: String,
    /// Drop-in forks tried if `command` isn't on PATH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_bins: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProbeResult {
    /// Original command requested.
    pub command: String,
    /// Absolute path that was found, or null.
    pub resolved_path: Option<String>,
    /// Which bin actually matched (command or one of the fallbacks).
    pub matched_bin: Option<String>,
}

/// `path.delimiter` of the given platform.
pub(crate) fn path_delimiter(windows: bool) -> char {
    if windows { ';' } else { ':' }
}

/// An environment variable as Node would hand it over: undecodable bytes are
/// replaced instead of making the lookup fail.
pub(crate) fn env_lossy(name: &str) -> Option<String> {
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

/// `os.homedir()` as text.
pub(crate) fn home_lossy() -> Option<String> {
    std::env::home_dir().map(|home| home.to_string_lossy().into_owned())
}

/// `String.prototype.trim`: Rust's `trim` also strips U+0085 and leaves U+FEFF.
pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

/// Windows PATHEXT values, lowercased, with '' for already-suffixed names.
pub fn windows_exts(pathext: Option<&str>) -> Vec<String> {
    let raw = pathext.unwrap_or(".COM;.EXE;.BAT;.CMD");
    std::iter::once(String::new())
        .chain(
            raw.split(';')
                .map(str::to_lowercase)
                .filter(|ext| !ext.is_empty()),
        )
        .collect()
}

/// PATH entries followed by `extra`.
pub fn path_dirs(path_env: Option<&str>, extra: Vec<String>, windows: bool) -> Vec<String> {
    path_env
        .unwrap_or_default()
        .split(path_delimiter(windows))
        .filter(|dir| !dir.is_empty())
        .map(str::to_owned)
        .chain(extra)
        .collect()
}

/// Augment PATH with locations commonly missing from a GUI app's env.
/// Without a home directory the home-relative entries are left out: joined to
/// nothing they would be relative paths, resolved against the working directory.
pub fn extra_dirs_in(
    home: Option<&str>,
    windows: bool,
    env: impl Fn(&str) -> Option<String>,
) -> Vec<String> {
    let under_home = |parts: &[&str]| {
        home.filter(|home| !home.is_empty()).map(|home| {
            let mut all = vec![home];
            all.extend_from_slice(parts);
            node_path::join(&all, windows)
        })
    };
    if windows {
        let var = |name: &str| env(name).filter(|value| !value.is_empty());
        let under = |dir: Option<String>, parts: &[&str]| {
            dir.map(|dir| {
                let mut all = vec![dir.as_str()];
                all.extend_from_slice(parts);
                node_path::join(&all, true)
            })
        };
        let program_files = var("ProgramW6432").or_else(|| var("ProgramFiles"));
        return [
            under(var("APPDATA"), &["npm"]),
            under_home(&["AppData", "Roaming", "npm"]),
            under_home(&["scoop", "shims"]),
            under_home(&[".bun", "bin"]),
            // npm's Windows CLI shims (claude-code-acp.cmd) run `node <adapter>` and
            // fall back to `node` on PATH when there's no node.exe beside the shim.
            // A GUI/packaged launch can hand the app a PATH without Node's install
            // dir, so the shim exits with "'node' is not recognized" the instant it
            // starts — the agent's stdout closes before the handshake and the SDK
            // reports the opaque "ACP connection closed". Add the common Node install
            // dirs so the shim can always resolve `node`.
            under(program_files, &["nodejs"]),
            under(var("ProgramFiles(x86)"), &["nodejs"]),
            under(var("LOCALAPPDATA"), &["Volta", "bin"]),
            // nvm-windows points this at its active version's dir (usually the
            // C:\Program Files\nodejs symlink, but honour an explicit override).
            var("NVM_SYMLINK"),
        ]
        .into_iter()
        .flatten()
        .collect();
    }
    [
        Some("/opt/homebrew/bin".to_owned()),
        Some("/usr/local/bin".to_owned()),
        Some("/usr/bin".to_owned()),
        under_home(&[".local", "bin"]),
        under_home(&[".bun", "bin"]),
        under_home(&[".cargo", "bin"]),
        under_home(&[".npm-global", "bin"]),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// [`extra_dirs_in`] for this process.
pub fn extra_dirs() -> Vec<String> {
    extra_dirs_in(home_lossy().as_deref(), cfg!(windows), env_lossy)
}

/// What Node's `access(p, X_OK)` answers: may this user execute (or, for a
/// directory, enter) the path. Mode bits alone would ignore who owns the file.
#[cfg(unix)]
pub fn is_executable(path: &str) -> bool {
    let Ok(c_path) = std::ffi::CString::new(path) else {
        return false;
    };
    // SAFETY: `c_path` is a NUL-terminated string that lives across the call.
    unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
}

/// Windows has no execute bit; the TypeScript only checks that the file exists.
#[cfg(not(unix))]
pub fn is_executable(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

/// Resolve a single bin name to an absolute path, or None if not found.
pub fn which(
    bin: &str,
    dirs: &[String],
    exts: &[String],
    windows: bool,
    is_executable: &dyn Fn(&str) -> bool,
) -> Option<String> {
    for dir in dirs {
        for ext in exts {
            let candidate = node_path::join(&[dir, &format!("{bin}{ext}")], windows);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// The directory `npm prefix -g` installs bins into, from the command's output.
pub fn npm_global_bin_dir(prefix_stdout: &str, windows: bool) -> Option<String> {
    let prefix = js_trim(prefix_stdout);
    if prefix.is_empty() {
        return None;
    }
    Some(if windows {
        prefix.to_owned()
    } else {
        node_path::join(&[prefix, "bin"], false)
    })
}

/// For each probe, tries `command` first, then `fallback_bins` in order.
pub fn resolve_probes(
    probes: &[AgentProbe],
    dirs: &[String],
    exts: &[String],
    windows: bool,
    is_executable: &dyn Fn(&str) -> bool,
) -> Vec<AgentProbeResult> {
    probes
        .iter()
        .map(|probe| {
            let fallbacks = probe.fallback_bins.as_deref().unwrap_or_default();
            let hit = std::iter::once(&probe.command)
                .chain(fallbacks)
                .find_map(|bin| Some((which(bin, dirs, exts, windows, is_executable)?, bin)));
            AgentProbeResult {
                command: probe.command.clone(),
                resolved_path: hit.as_ref().map(|(path, _)| path.clone()),
                matched_bin: hit.map(|(_, bin)| bin.clone()),
            }
        })
        .collect()
}

/// Resolve a list of agent probes against PATH. Never spawns the agent
/// binaries, just stat-checks files.
pub fn detect_agents(probes: &[AgentProbe]) -> Vec<AgentProbeResult> {
    let windows = cfg!(windows);
    let mut dirs = path_dirs(env_lossy("PATH").as_deref(), extra_dirs(), windows);
    let exts = if windows {
        windows_exts(env_lossy("PATHEXT").as_deref())
    } else {
        vec![String::new()]
    };

    // Also try `npm prefix -g` once and prepend its bin/ — picks up custom
    // npm prefixes (nvm, volta) that aren't on PATH inside a GUI app.
    // npm not on PATH — skip silently.
    let npm = if windows { "npm.cmd" } else { "npm" };
    let prefix = exec_file_stdout(npm, &["prefix", "-g"], Duration::from_millis(2_000));
    if let Some(dir) = prefix.and_then(|out| npm_global_bin_dir(&out, windows)) {
        dirs.insert(0, dir);
    }

    resolve_probes(probes, &dirs, &exts, windows, &is_executable)
}

/// Node's default `maxBuffer` for `execFile`.
const EXEC_MAX_BUFFER: u64 = 1024 * 1024;

/// `execFile` with a timeout, keeping only stdout. None when the program is
/// missing, exits non-zero, outlives `timeout` or writes more than 1 MiB.
pub(crate) fn exec_file_stdout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;

    // Read on a thread: std has no read timeout, and a grandchild that keeps the
    // pipe open must not hold this call past the deadline.
    let (sender, receiver) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let read = stdout.take(EXEC_MAX_BUFFER + 1).read_to_end(&mut buffer);
            let _ = sender.send(read.ok().map(|_| buffer));
        });
    }
    let output = receiver
        .recv_timeout(timeout)
        .ok()
        .flatten()
        .filter(|buffer| buffer.len() as u64 <= EXEC_MAX_BUFFER);

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if output.is_some() && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => break None,
        }
    };
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    status
        .filter(std::process::ExitStatus::success)
        .and(output)
        .map(|buffer| String::from_utf8_lossy(&buffer).into_owned())
}

/// The parts of Node's `path` module these ports need, for either platform's
/// rules regardless of the host: the TypeScript picks `posix` or `win32`
/// explicitly, and its tests run the Windows branch on any machine.
pub(crate) mod node_path {
    fn is_sep(byte: u8, windows: bool) -> bool {
        byte == b'/' || (windows && byte == b'\\')
    }

    /// Never panics: every index used here sits next to an ASCII separator.
    fn slice(path: &str, from: usize, to: usize) -> &str {
        path.get(from..to).unwrap_or_default()
    }

    /// Lexical `.` / `..` resolution (Node's `normalizeString`).
    fn normalize_segments(path: &str, allow_above_root: bool, windows: bool) -> String {
        let mut kept: Vec<&str> = Vec::new();
        for segment in path.split(|c| c == '/' || (windows && c == '\\')) {
            match segment {
                "" | "." => {}
                ".." => match kept.last() {
                    Some(&last) if last != ".." => {
                        kept.pop();
                    }
                    _ if allow_above_root => kept.push(".."),
                    _ => {}
                },
                other => kept.push(other),
            }
        }
        kept.join(if windows { "\\" } else { "/" })
    }

    fn normalize_posix(path: &str) -> String {
        if path.is_empty() {
            return ".".to_owned();
        }
        let absolute = path.starts_with('/');
        let trailing = path.ends_with('/');
        let mut out = normalize_segments(path, !absolute, false);
        if out.is_empty() {
            let bare = if absolute {
                "/"
            } else if trailing {
                "./"
            } else {
                "."
            };
            return bare.to_owned();
        }
        if trailing {
            out.push('/');
        }
        if absolute { format!("/{out}") } else { out }
    }

    // shortcut: skips Node's handling of reserved device names (`CON:`, `\\?\COM1:`),
    // which it returns unnormalized; add it if a path built here is ever used to
    // confine file access.
    fn normalize_win32(path: &str) -> String {
        let bytes = path.as_bytes();
        let len = bytes.len();
        let sep = |at: usize| bytes.get(at).is_some_and(|byte| is_sep(*byte, true));
        // End of the run of separators (or non-separators) starting at `from`.
        let run = |from: usize, separators: bool| {
            (from..len).find(|at| sep(*at) != separators).unwrap_or(len)
        };
        match len {
            0 => return ".".to_owned(),
            1 if sep(0) => return "\\".to_owned(),
            1 => return path.to_owned(),
            _ => {}
        }

        let mut root_end = 0;
        let mut device: Option<String> = None;
        let mut absolute = false;
        if sep(0) {
            absolute = true;
            if sep(1) {
                // Possible UNC root: \\server\share
                let server_end = run(2, false);
                let share_start = run(server_end, true);
                if server_end > 2 && share_start < len {
                    let server = slice(path, 2, server_end);
                    let share_end = run(share_start, false);
                    if server == "." || server == "?" {
                        // Device root such as \\.\PHYSICALDRIVE0
                        device = Some(format!("\\\\{server}"));
                        root_end = 4;
                    } else if share_end == len {
                        return format!("\\\\{server}\\{}\\", slice(path, share_start, len));
                    } else {
                        let share = slice(path, share_start, share_end);
                        device = Some(format!("\\\\{server}\\{share}"));
                        root_end = share_end;
                    }
                }
            } else {
                root_end = 1;
            }
        } else if bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':')
        {
            device = Some(slice(path, 0, 2).to_owned());
            root_end = 2;
            // A separator after the drive name marks an absolute path.
            if sep(2) {
                absolute = true;
                root_end = 3;
            }
        }

        let mut tail = normalize_segments(slice(path, root_end, len), !absolute, true);
        if tail.is_empty() && !absolute {
            tail.push('.');
        }
        if !tail.is_empty() && sep(len - 1) {
            tail.push('\\');
        }
        // A relative path with no drive must not come out as something Windows
        // reads as absolute; Node keeps it relative with `.\` (CVE-2024-36139).
        if !absolute && device.is_none() {
            let drive_like = tail.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && tail.as_bytes().get(1) == Some(&b':');
            let colon_ends_segment =
                (0..len).any(|at| bytes.get(at) == Some(&b':') && (at + 1 == len || sep(at + 1)));
            if drive_like || colon_ends_segment {
                return format!(".\\{tail}");
            }
        }
        let device = device.unwrap_or_default();
        if absolute {
            format!("{device}\\{tail}")
        } else {
            format!("{device}{tail}")
        }
    }

    /// `path.posix.join` / `path.win32.join`.
    pub(crate) fn join(parts: &[&str], windows: bool) -> String {
        let parts: Vec<&str> = parts.iter().copied().filter(|p| !p.is_empty()).collect();
        let Some(first) = parts.first() else {
            return ".".to_owned();
        };
        if !windows {
            return normalize_posix(&parts.join("/"));
        }
        let mut joined = parts.join("\\");

        // The joined path must not start with two slashes, or normalize would
        // take it for a UNC path. Skipped when the first part clearly is one:
        // exactly two slashes followed by something else.
        let first = first.as_bytes();
        let first_sep = |at: usize| first.get(at).is_some_and(|byte| is_sep(*byte, true));
        let mut slashes = 0;
        let mut needs_replace = true;
        if first_sep(0) {
            slashes = 1;
            if first_sep(1) {
                slashes = 2;
                if first_sep(2) {
                    slashes = 3;
                } else if first.len() > 2 {
                    needs_replace = false;
                }
            }
        }
        if needs_replace {
            let rest = slice(&joined, slashes, joined.len());
            let rest = rest.trim_start_matches(['/', '\\']);
            if joined.len() - rest.len() >= 2 {
                joined = format!("\\{rest}");
            }
        }
        normalize_win32(&joined)
    }

    /// `path.posix.isAbsolute` / `path.win32.isAbsolute`.
    pub(crate) fn is_absolute(path: &str, windows: bool) -> bool {
        let bytes = path.as_bytes();
        let sep = |at: usize| bytes.get(at).is_some_and(|byte| is_sep(*byte, windows));
        sep(0)
            || (windows
                && bytes.first().is_some_and(u8::is_ascii_alphabetic)
                && bytes.get(1) == Some(&b':')
                && sep(2))
    }

    /// `path.posix.dirname` / `path.win32.dirname`.
    pub(crate) fn dirname(path: &str, windows: bool) -> String {
        let bytes = path.as_bytes();
        let len = bytes.len();
        let sep = |at: usize| bytes.get(at).is_some_and(|byte| is_sep(*byte, windows));
        let run = |from: usize, separators: bool| {
            (from..len).find(|at| sep(*at) != separators).unwrap_or(len)
        };
        if len == 0 {
            return ".".to_owned();
        }

        // Where the root ends (None: no root) and where the scan may stop.
        let mut root_end: Option<usize> = None;
        let mut offset = 0;
        if !windows {
            if sep(0) {
                root_end = Some(1);
                offset = 1;
            }
        } else if len == 1 {
            return if sep(0) { path } else { "." }.to_owned();
        } else if sep(0) {
            root_end = Some(1);
            offset = 1;
            if sep(1) {
                // UNC root: \\server\share\ belongs to the root.
                let server_end = run(2, false);
                let share_start = run(server_end, true);
                if server_end > 2 && share_start < len {
                    let share_end = run(share_start, false);
                    if share_end == len {
                        return path.to_owned();
                    }
                    root_end = Some(share_end + 1);
                    offset = share_end + 1;
                }
            }
        } else if bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':')
        {
            let end = if sep(2) { 3 } else { 2 };
            root_end = Some(end);
            offset = end;
        }

        // The separator before the last segment, ignoring trailing separators.
        let mut end = None;
        let mut matched_slash = true;
        for at in (offset.max(usize::from(!windows))..len).rev() {
            if !sep(at) {
                matched_slash = false;
            } else if !matched_slash {
                end = Some(at);
                break;
            }
        }
        match (end, root_end) {
            (None, None) => ".".to_owned(),
            (None, Some(root_end)) => slice(path, 0, root_end).to_owned(),
            (Some(1), Some(_)) if !windows => "//".to_owned(),
            (Some(end), _) => slice(path, 0, end).to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::node_path::{dirname, is_absolute, join};
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    // ── node:path ── expected values are what Node 24 prints for the same call.

    #[test]
    fn posix_join은_node와_같은_결과를_낸다() {
        let cases: &[(&[&str], &str)] = &[
            (&["/usr/bin/", "claude"], "/usr/bin/claude"),
            (&["/a//b/../c", "claude"], "/a/c/claude"),
            (&[".", "claude"], "claude"),
            (&["rel/./x", "claude"], "rel/x/claude"),
            (&["/..", "claude"], "/claude"),
            (&["../..", "x"], "../../x"),
            (&["", ""], "."),
            (&["/", "bin"], "/bin"),
            (&["/home/u", ".local", "bin"], "/home/u/.local/bin"),
            (&["/a", "b/"], "/a/b/"),
            (&["a/..", ".."], ".."),
        ];
        for (parts, expected) in cases {
            assert_eq!(join(parts, false), *expected, "{parts:?}");
        }
    }

    #[test]
    fn win32_join은_node와_같은_결과를_낸다() {
        let cases: &[(&[&str], &str)] = &[
            (&["C:\\npm", "claude.cmd"], "C:\\npm\\claude.cmd"),
            (&["C:/npm/", "claude.cmd"], "C:\\npm\\claude.cmd"),
            (&["C:", "x"], "C:\\x"),
            (&["C:\\", "x"], "C:\\x"),
            (
                &["\\\\server\\share\\dir", "x"],
                "\\\\server\\share\\dir\\x",
            ),
            (&["\\\\server\\share", "x"], "\\\\server\\share\\x"),
            (&["\\\\server", "x"], "\\\\server\\x\\"),
            (&["C:\\a\\..\\..\\b", "x"], "C:\\b\\x"),
            (&["C:a\\..\\..", "x"], "C:..\\x"),
            (&["\\a\\.\\b", "x"], "\\a\\b\\x"),
            (&["rel\\dir", "x"], "rel\\dir\\x"),
            (
                &["C:\\Users\\u", "AppData", "Roaming", "npm"],
                "C:\\Users\\u\\AppData\\Roaming\\npm",
            ),
            (&["//server/share/..", "x"], "\\\\server\\share\\x"),
            (&["\\\\\\a", "x"], "\\a\\x"),
            (&["..", "x"], "..\\x"),
            (&["C:\\a\\", "b\\"], "C:\\a\\b\\"),
            (&["c:", ""], "c:."),
            (&["\\\\?\\C:\\a", "x"], "\\\\?\\C:\\a\\x"),
            (&["\\\\server\\\\share\\", "x"], "\\\\server\\share\\x"),
            // A colon in a relative path: kept relative with a leading `.\`.
            (&[".", "C:\\claude.exe"], ".\\C:\\claude.exe"),
            (&["rel", "x:"], ".\\rel\\x:"),
            (&["a/b:/c", "d"], ".\\a\\b:\\c\\d"),
            (&["1:\\x", "y"], ".\\1:\\x\\y"),
            (&["..", "C:x"], "..\\C:x"),
            (&["a", "b:c", "d"], "a\\b:c\\d"),
            (&["dir:with", "claude.cmd"], "dir:with\\claude.cmd"),
            (&["C:rel", "x"], "C:rel\\x"),
            (&["a:", "b"], "a:\\b"),
        ];
        for (parts, expected) in cases {
            assert_eq!(join(parts, true), *expected, "{parts:?}");
        }
    }

    #[test]
    fn dirname은_node와_같은_결과를_낸다() {
        let posix = [
            ("/usr/local/bin/codex-acp", "/usr/local/bin"),
            ("/x", "/"),
            ("/a//b", "/a/"),
            ("/a/b/", "/a"),
            ("/", "/"),
            ("//x", "//"),
            ("//", "/"),
        ];
        for (path, expected) in posix {
            assert_eq!(dirname(path, false), expected, "{path:?}");
        }
        let win32 = [
            ("C:\\npm\\claude.cmd", "C:\\npm"),
            ("C:\\claude.cmd", "C:\\"),
            ("C:/npm/x", "C:/npm"),
            ("\\\\server\\share\\x", "\\\\server\\share\\"),
            ("\\\\server\\share\\d\\x", "\\\\server\\share\\d"),
            ("\\x", "\\"),
            ("\\a\\b\\", "\\a"),
            ("\\\\server\\share", "\\\\server\\share"),
            ("\\", "\\"),
            ("C:\\", "C:\\"),
        ];
        for (path, expected) in win32 {
            assert_eq!(dirname(path, true), expected, "{path:?}");
        }
    }

    #[test]
    fn is_absolute는_플랫폼_규칙을_따른다() {
        for (path, expected) in [
            ("C:\\x", true),
            ("C:x", false),
            ("\\x", true),
            ("/x", true),
            ("x", false),
            ("C:", false),
            ("1:\\x", false),
            ("", false),
        ] {
            assert_eq!(is_absolute(path, true), expected, "{path:?}");
        }
        assert!(is_absolute("/usr/bin/x", false));
        assert!(!is_absolute("C:\\x", false));
        assert!(!is_absolute("\\x", false));
        assert!(!is_absolute("", false));
    }

    #[test]
    fn 멀티바이트_경로에서도_패닉하지_않는다() {
        assert_eq!(join(&["/홈/사용자", "bin"], false), "/홈/사용자/bin");
        assert_eq!(join(&["C:\\사용자\\", "npm"], true), "C:\\사용자\\npm");
        assert_eq!(join(&["한", "x"], true), "한\\x");
        assert_eq!(dirname("/홈/사용자/claude", false), "/홈/사용자");
        assert_eq!(dirname("한글", true), ".");
        assert!(!is_absolute("한:\\x", true));
    }

    // ── PATH pieces ──

    #[test]
    fn pathext가_없으면_기본_확장자를_쓴다() {
        assert_eq!(
            windows_exts(None),
            strings(&["", ".com", ".exe", ".bat", ".cmd"])
        );
    }

    #[test]
    fn pathext는_소문자로_바꾸고_빈_항목을_버린다() {
        assert_eq!(
            windows_exts(Some(".EXE;;.Ps1;")),
            strings(&["", ".exe", ".ps1"])
        );
        // An empty PATHEXT is still "set": only the bare name is tried.
        assert_eq!(windows_exts(Some("")), strings(&[""]));
    }

    #[test]
    fn path를_구분자로_나누고_빈_항목을_버린_뒤_추가_디렉터리를_붙인다() {
        assert_eq!(
            path_dirs(Some("/a::/b:"), strings(&["/extra"]), false),
            strings(&["/a", "/b", "/extra"])
        );
        assert_eq!(
            path_dirs(Some("C:\\a;;D:\\b"), Vec::new(), true),
            strings(&["C:\\a", "D:\\b"])
        );
        assert_eq!(
            path_dirs(None, strings(&["/extra"]), false),
            strings(&["/extra"])
        );
    }

    #[test]
    fn posix_추가_디렉터리는_고정_위치와_홈_아래_bin들이다() {
        assert_eq!(
            extra_dirs_in(Some("/home/u"), false, |_| None),
            strings(&[
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/usr/bin",
                "/home/u/.local/bin",
                "/home/u/.bun/bin",
                "/home/u/.cargo/bin",
                "/home/u/.npm-global/bin",
            ])
        );
    }

    #[test]
    fn 홈_디렉터리를_모르면_홈_아래_항목은_넣지_않는다() {
        assert_eq!(
            extra_dirs_in(None, false, |_| None),
            strings(&["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"])
        );
        assert!(extra_dirs_in(None, true, |_| None).is_empty());
    }

    #[test]
    fn windows_추가_디렉터리는_환경변수가_있는_것만_넣는다() {
        let env = |name: &str| match name {
            "APPDATA" => Some("C:\\Users\\u\\AppData\\Roaming".to_owned()),
            "ProgramW6432" => Some("C:\\Program Files".to_owned()),
            "ProgramFiles" => Some("C:\\Program Files (Arm)".to_owned()),
            "ProgramFiles(x86)" => Some("C:\\Program Files (x86)".to_owned()),
            "LOCALAPPDATA" => Some("C:\\Users\\u\\AppData\\Local".to_owned()),
            "NVM_SYMLINK" => Some("D:\\nvm\\current".to_owned()),
            _ => None,
        };
        assert_eq!(
            extra_dirs_in(Some("C:\\Users\\u"), true, env),
            strings(&[
                "C:\\Users\\u\\AppData\\Roaming\\npm",
                "C:\\Users\\u\\AppData\\Roaming\\npm",
                "C:\\Users\\u\\scoop\\shims",
                "C:\\Users\\u\\.bun\\bin",
                "C:\\Program Files\\nodejs",
                "C:\\Program Files (x86)\\nodejs",
                "C:\\Users\\u\\AppData\\Local\\Volta\\bin",
                "D:\\nvm\\current",
            ])
        );
        assert_eq!(
            extra_dirs_in(Some("C:\\Users\\u"), true, |_| None),
            strings(&[
                "C:\\Users\\u\\AppData\\Roaming\\npm",
                "C:\\Users\\u\\scoop\\shims",
                "C:\\Users\\u\\.bun\\bin",
            ])
        );
    }

    #[test]
    fn programw6432가_비어_있으면_programfiles를_쓴다() {
        let env = |name: &str| match name {
            "ProgramW6432" => Some(String::new()),
            "ProgramFiles" => Some("C:\\PF".to_owned()),
            _ => None,
        };
        assert_eq!(extra_dirs_in(None, true, env), strings(&["C:\\PF\\nodejs"]));
    }

    // ── which / probes ──

    #[test]
    fn which는_디렉터리_순서_다음_확장자_순서로_찾는다() {
        let tried = std::cell::RefCell::new(Vec::new());
        let found = which(
            "codex",
            &strings(&["C:\\a", "C:\\b"]),
            &strings(&["", ".exe", ".cmd"]),
            true,
            &|path| {
                tried.borrow_mut().push(path.to_owned());
                path == "C:\\b\\codex.exe"
            },
        );
        assert_eq!(found.as_deref(), Some("C:\\b\\codex.exe"));
        assert_eq!(
            *tried.borrow(),
            strings(&[
                "C:\\a\\codex",
                "C:\\a\\codex.exe",
                "C:\\a\\codex.cmd",
                "C:\\b\\codex",
                "C:\\b\\codex.exe",
            ])
        );
    }

    #[test]
    fn which는_없으면_none을_돌려준다() {
        let dirs = strings(&["/a", "/b"]);
        assert_eq!(
            which("codex", &dirs, &strings(&[""]), false, &|_| false),
            None
        );
        assert_eq!(which("codex", &[], &strings(&[""]), false, &|_| true), None);
    }

    #[test]
    fn 프로브는_command를_먼저_보고_다음에_fallback을_순서대로_본다() {
        let probes = [
            AgentProbe {
                command: "claude-agent-acp".to_owned(),
                fallback_bins: Some(strings(&["claude-code-acp", "other-acp"])),
            },
            AgentProbe {
                command: "codex-acp".to_owned(),
                fallback_bins: None,
            },
            AgentProbe {
                command: "missing".to_owned(),
                fallback_bins: Some(strings(&["also-missing"])),
            },
        ];
        let exists = |path: &str| {
            [
                "/b/claude-code-acp",
                "/b/other-acp",
                "/a/codex-acp",
                "/b/codex-acp",
            ]
            .contains(&path)
        };
        let results = resolve_probes(
            &probes,
            &strings(&["/a", "/b"]),
            &strings(&[""]),
            false,
            &exists,
        );
        assert_eq!(
            serde_json::to_value(&results).ok(),
            Some(serde_json::json!([
                {
                    "command": "claude-agent-acp",
                    "resolvedPath": "/b/claude-code-acp",
                    "matchedBin": "claude-code-acp"
                },
                { "command": "codex-acp", "resolvedPath": "/a/codex-acp", "matchedBin": "codex-acp" },
                { "command": "missing", "resolvedPath": null, "matchedBin": null }
            ]))
        );
    }

    #[test]
    fn 프로브_json은_fallback_bins가_없거나_null이어도_읽힌다() {
        let probes: Vec<AgentProbe> = serde_json::from_str(
            r#"[{"command":"a"},{"command":"b","fallbackBins":null},{"command":"c","fallbackBins":["d"]}]"#,
        )
        .unwrap_or_default();
        let fallbacks: Vec<_> = probes.iter().map(|p| p.fallback_bins.clone()).collect();
        assert_eq!(fallbacks, [None, None, Some(strings(&["d"]))]);
    }

    #[test]
    fn npm_prefix_출력에서_전역_bin_디렉터리를_만든다() {
        assert_eq!(
            npm_global_bin_dir("/Users/u/.nvm/versions/node/v24\n", false).as_deref(),
            Some("/Users/u/.nvm/versions/node/v24/bin")
        );
        // On Windows the shims sit in the prefix itself.
        assert_eq!(
            npm_global_bin_dir("C:\\Users\\u\\AppData\\Roaming\\npm\r\n", true).as_deref(),
            Some("C:\\Users\\u\\AppData\\Roaming\\npm")
        );
        assert_eq!(npm_global_bin_dir(" \n", false), None);
        assert_eq!(npm_global_bin_dir("", true), None);
    }

    #[test]
    fn js_trim은_자바스크립트_공백만_잘라낸다() {
        assert_eq!(js_trim(" \t\r\n\u{a0}\u{feff}x y\u{2028}\u{3000}"), "x y");
        assert_eq!(js_trim("\u{85}x\u{85}"), "\u{85}x\u{85}");
    }

    // ── real filesystem and processes ──

    #[cfg(unix)]
    #[test]
    fn 실제_환경에서_path와_추가_디렉터리의_실행_파일을_찾는다() {
        let missing = "rever-no-such-binary-for-tests";
        let probes = [
            AgentProbe {
                command: missing.to_owned(),
                fallback_bins: Some(strings(&["env"])),
            },
            AgentProbe {
                command: missing.to_owned(),
                fallback_bins: None,
            },
        ];
        let results = detect_agents(&probes);
        assert_eq!(results.len(), 2);
        // /usr/bin is always searched, whatever PATH the test runs with.
        let found = results.first();
        assert_eq!(found.map(|r| r.command.as_str()), Some(missing));
        assert_eq!(found.and_then(|r| r.matched_bin.as_deref()), Some("env"));
        let path = found.and_then(|r| r.resolved_path.as_deref());
        assert!(path.is_some_and(|path| path.ends_with("/env") && is_executable(path)));
        assert_eq!(
            results.get(1),
            Some(&AgentProbeResult {
                command: missing.to_owned(),
                resolved_path: None,
                matched_bin: None,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn 실행_권한이_있는_파일만_실행_가능으로_본다() {
        use std::os::unix::fs::PermissionsExt;

        let dir = crate::test_support::TempDir::new("is-executable");
        let file = dir.path().join("tool");
        let text = |path: &std::path::Path| path.to_string_lossy().into_owned();
        assert!(!is_executable(&text(&file)), "missing file");

        assert!(std::fs::write(&file, "#!/bin/sh\n").is_ok());
        assert!(std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).is_ok());
        assert!(!is_executable(&text(&file)), "no execute bit");

        assert!(std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).is_ok());
        assert!(is_executable(&text(&file)));
        // access(X_OK) on a directory means "may enter", as in the TypeScript.
        assert!(is_executable(&text(dir.path())));
        assert!(!is_executable("bad\0path"));
    }

    #[cfg(unix)]
    #[test]
    fn exec_file_stdout은_성공한_프로세스의_출력만_돌려준다() {
        let second = Duration::from_secs(5);
        assert_eq!(
            exec_file_stdout("sh", &["-c", "printf ' hi\\n'"], second).as_deref(),
            Some(" hi\n")
        );
        assert_eq!(
            exec_file_stdout("sh", &["-c", "echo no; exit 3"], second),
            None
        );
        assert_eq!(
            exec_file_stdout("rever-no-such-binary-for-tests", &[], second),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn exec_file_stdout은_제한_시간을_넘기면_프로세스를_끝낸다() {
        let started = Instant::now();
        let out = exec_file_stdout("sh", &["-c", "sleep 30"], Duration::from_millis(150));
        assert_eq!(out, None);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn exec_file_stdout은_1mib를_넘는_출력을_버린다() {
        let script = "head -c 1048577 /dev/zero";
        assert_eq!(
            exec_file_stdout("sh", &["-c", script], Duration::from_secs(10)),
            None
        );
        let script = "head -c 1048576 /dev/zero";
        let out = exec_file_stdout("sh", &["-c", script], Duration::from_secs(10));
        assert_eq!(out.map(|text| text.len()), Some(1_048_576));
    }
}
