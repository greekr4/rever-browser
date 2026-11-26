//! Port of `src/main/win-shell.ts` — quoting for agent CLIs launched through cmd.exe.
//!
//! Windows has to launch agent CLIs through cmd.exe: npm installs them as
//! `.cmd` shims, and Node's CVE-2024-27980 patch refuses to spawn `.cmd`/`.bat`
//! without `shell: true` (EINVAL). But with `shell: true` Node just joins
//! `[command, ...args]` with spaces and hands the line to `cmd.exe /d /s /c`,
//! unquoted — so an empty argument vanishes (`--setting-sources ''` became
//! `--setting-sources --permission-mode`), JSON loses its quotes, and paths with
//! spaces split. Quote everything ourselves so each argument arrives intact.

/// cmd.exe metacharacters (same set cross-spawn escapes). `"` is included so
/// cmd never toggles its quote state mid-argument: every char is literal to cmd.
const CMD_META: &str = "()][%!^\"`<>&|;, *?";

/// Quote one argument for a cmd.exe command line: first by the MSVCRT /
/// CommandLineToArgvW rules the target program parses with, then caret-escape
/// cmd's metacharacters so cmd passes it through untouched.
pub fn quote_cmd_arg(arg: &str) -> String {
    let mut msvcrt = String::from('"');
    // Backslashes are literal to MSVCRT unless a quote follows the run, so a run
    // is doubled only before an embedded quote and before the closing quote.
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                msvcrt.extend(std::iter::repeat_n('\\', backslashes));
                msvcrt.push('\\');
                backslashes = 0;
            }
            _ => backslashes = 0,
        }
        msvcrt.push(ch);
    }
    msvcrt.extend(std::iter::repeat_n('\\', backslashes));
    msvcrt.push('"');

    let mut quoted = String::with_capacity(msvcrt.len() * 2);
    for ch in msvcrt.chars() {
        if CMD_META.contains(ch) {
            quoted.push('^');
        }
        quoted.push(ch);
    }
    quoted
}

/// Quote the program path itself (it may live under `C:\Program Files`).
pub fn quote_cmd_command(command: &str) -> String {
    format!("\"{}\"", command.replace('"', ""))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnPlan {
    pub command: String,
    pub args: Vec<String>,
    pub shell: bool,
}

/// `spawn` arguments that survive the platform's launch path: on Windows the
/// command line is pre-quoted for cmd.exe and run with `shell: true`; elsewhere
/// the args go straight to the executable with no shell.
/// Callers pass `cfg!(windows)` for `windows`.
pub fn shell_safe_spawn(command: &str, args: &[impl AsRef<str>], windows: bool) -> SpawnPlan {
    if !windows {
        return SpawnPlan {
            command: command.to_owned(),
            args: args.iter().map(|arg| arg.as_ref().to_owned()).collect(),
            shell: false,
        };
    }
    // One pre-built command line with no args array: Node would only join them
    // with spaces anyway, and passing args with `shell: true` is deprecated (DEP0190).
    let mut line = quote_cmd_command(command);
    for arg in args {
        line.push(' ');
        line.push_str(&quote_cmd_arg(arg.as_ref()));
    }
    SpawnPlan {
        command: line,
        args: Vec::new(),
        shell: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── shellSafeSpawn ──

    #[test]
    fn posix_실행은_그대로_둔다() {
        assert_eq!(
            shell_safe_spawn("claude", &["--setting-sources", ""], false),
            SpawnPlan {
                command: "claude".to_owned(),
                args: vec!["--setting-sources".to_owned(), String::new()],
                shell: false,
            }
        );
    }

    #[test]
    fn windows에서_빈_인자를_따옴표_쌍으로_남긴다() {
        assert_eq!(quote_cmd_arg(""), "^\"^\"");
        assert_eq!(
            shell_safe_spawn(
                "C:\\Program Files\\claude.exe",
                &["--setting-sources", ""],
                true
            ),
            SpawnPlan {
                command: "\"C:\\Program Files\\claude.exe\" ^\"--setting-sources^\" ^\"^\""
                    .to_owned(),
                args: Vec::new(),
                shell: true,
            }
        );
    }

    #[test]
    fn 포함된_따옴표와_끝의_백슬래시를_msvcrt_규칙대로_이스케이프한다() {
        assert_eq!(quote_cmd_arg("a\"b"), "^\"a\\^\"b^\"");
        assert_eq!(quote_cmd_arg("dir\\"), "^\"dir\\\\^\"");
    }

    /// Real cmd.exe round trip: what the child sees in argv must equal what we passed.
    /// The TypeScript echoes argv with node; this builds a one-line echo program with rustc.
    #[cfg(windows)]
    mod cmd_round_trip {
        use std::os::windows::process::CommandExt;
        use std::path::PathBuf;
        use std::process::Command;

        use super::super::shell_safe_spawn;
        use crate::claude_cli_protocol::claude_cli_args;

        const SEPARATOR: char = '\u{1f}';

        /// A directory with spaces in its path, holding the echo program; removed on drop.
        struct Fixture {
            dir: PathBuf,
            echo: PathBuf,
        }

        impl Fixture {
            fn new(tag: &str) -> Self {
                let dir = std::env::temp_dir()
                    .join(format!("rever win shell {tag} {}", std::process::id()));
                std::fs::create_dir_all(&dir).unwrap();
                let source = dir.join("echo-argv.rs");
                std::fs::write(
                    &source,
                    r#"fn main() { for a in std::env::args().skip(1) { print!("{a}\u{1f}") } }"#,
                )
                .unwrap();
                let echo = dir.join("echo-argv.exe");
                let built = Command::new("rustc")
                    .arg(&source)
                    .arg("-o")
                    .arg(&echo)
                    .current_dir(&dir)
                    .output()
                    .unwrap();
                assert!(
                    built.status.success(),
                    "{}",
                    String::from_utf8_lossy(&built.stderr)
                );
                Self { dir, echo }
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }

        fn args() -> Vec<String> {
            let mut args = claude_cli_args(None);
            args.extend(
                [
                    "",
                    "x & y | z",
                    "%PATH%",
                    "say \"hi\" (now)!",
                    "C:\\dir with space\\",
                ]
                .map(str::to_owned),
            );
            args
        }

        fn run(command: &str, argv: &[String]) -> Vec<String> {
            let launch = shell_safe_spawn(command, argv, true);
            assert!(launch.shell);
            assert!(launch.args.is_empty());
            // What Node's `shell: true` runs: `cmd.exe /d /s /c "<line>"`, the line verbatim.
            let out = Command::new("cmd.exe")
                .raw_arg(format!("/d /s /c \"{}\"", launch.command))
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stderr), "");
            let stdout = String::from_utf8(out.stdout).unwrap();
            let mut seen: Vec<String> = stdout.split(SEPARATOR).map(str::to_owned).collect();
            // The echo program ends every argument with a separator.
            assert_eq!(seen.pop().as_deref(), Some(""));
            seen
        }

        #[test]
        fn exe에_모든_인자를_그대로_넘긴다() {
            let fixture = Fixture::new("exe");
            let args = args();
            assert_eq!(run(fixture.echo.to_str().unwrap(), &args), args);
        }

        #[test]
        fn npm식_cmd_shim을_거쳐도_모든_인자를_그대로_넘긴다() {
            let fixture = Fixture::new("shim");
            // Same shape as the shim npm writes for a global CLI.
            let shim = fixture.dir.join("echo argv.cmd");
            std::fs::write(&shim, format!("@\"{}\" %*\r\n", fixture.echo.display())).unwrap();
            // %VAR% inside a batch %* line is re-expanded by cmd; that is a shim limit, not ours.
            let via_shim: Vec<String> = args().into_iter().filter(|a| !a.contains('%')).collect();
            assert_eq!(run(shim.to_str().unwrap(), &via_shim), via_shim);
        }
    }
}
