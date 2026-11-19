//! Port of `src/main/mcp/script-analysis.ts` — helpers behind the script tools:
//! listing captured scripts, grepping their bodies, guessing the bundler and
//! deobfuscating through the external `webcrack` CLI.
//!
//! Offsets, snippet widths and size limits count UTF-16 code units, as the
//! TypeScript's string indices do. Patterns are `regex::Regex`: it has no
//! global flag (every search here is global), no lookaround and no backreferences.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;
use rever_core::traffic::{ListFilter, StoredRequest, TrafficStore};
use serde::{Deserialize, Serialize};

pub use crate::grep_worker::GrepTimeoutError;
use crate::grep_worker::{GREP_TIMEOUT_MS, RawMatch, Utf16Offsets, exec_all, exec_regex};
use crate::{JS_SPACE_CLASS, utf16_len, utf16_prefix_len, utf16_suffix_start};

#[derive(Clone, Debug, Default)]
pub struct ScriptFilter {
    pub host: Option<String>,
    pub limit: Option<usize>,
    pub min_size: Option<u64>,
}

pub fn list_scripts<'a>(store: &'a TrafficStore, filter: &ScriptFilter) -> Vec<&'a StoredRequest> {
    let limit = filter.limit.unwrap_or(50);
    let rows = store.list(&ListFilter {
        method_or_type: Some("Script".into()),
        host: filter.host.clone(),
        limit: Some(limit.max(50)),
        ..ListFilter::default()
    });
    let min_size = filter.min_size.unwrap_or(0);
    let mut sized: Vec<(usize, &StoredRequest)> = rows
        .into_iter()
        .filter_map(|r| {
            let body = r.response_body.as_deref().filter(|b| !b.is_empty())?;
            let len = utf16_len(body);
            (r.response_body_base64 != Some(true)
                && r.encoded_data_length.unwrap_or(len as u64) >= min_size)
                .then_some((len, r))
        })
        .collect();
    // Stable, like Array.prototype.sort: equal sizes stay newest first.
    sized.sort_by_key(|(len, _)| Reverse(*len));
    sized.into_iter().take(limit).map(|(_, r)| r).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    Api,
    Urls,
    Env,
    Secrets,
    Auth,
    Hooks,
    Fetch,
    Ai,
    Baas,
    FunctionCalling,
    Rpc,
}

// The TypeScript sources, with three adjustments so they match what JavaScript
// matches: `\b` only knows ASCII word characters there, `\s` is JavaScript's
// whitespace set, and the `i` flag folds ASCII letters only.
fn js_regex(source: &str) -> Regex {
    Regex::new(
        &source
            .replace(r"\b", r"(?-u:\b)")
            .replace(r"\s", JS_SPACE_CLASS),
    )
    .expect("the patterns are constants")
}

struct CategoryPatterns {
    api: Regex,
    urls: Regex,
    env: Regex,
    secrets: Regex,
    auth: Regex,
    hooks: Regex,
    fetch: Regex,
    ai: Regex,
    baas: Regex,
    function_calling: Regex,
    rpc: Regex,
}

static CATEGORY_PATTERNS: LazyLock<CategoryPatterns> = LazyLock::new(|| CategoryPatterns {
    api: js_regex(r#"["'`](/api/[^"'`]+|[^"'`]*/(?:v1|v2|v3)/[^"'`]+)["'`]"#),
    urls: js_regex(r"https?://[a-zA-Z0-9._:/?#@!$&'()*+,;=~%-]+"),
    env: js_regex(r"NEXT_PUBLIC_[A-Z0-9_]+|process\.env\.[A-Z0-9_]+|VITE_[A-Z0-9_]+"),
    secrets: js_regex(
        r"eyJ[a-zA-Z0-9_-]{20,}|sb_publishable_[a-zA-Z0-9_-]+|AIza[a-zA-Z0-9_-]{30,}",
    ),
    auth: js_regex(
        r"accessToken|refreshToken|signIn|signOut|signUp|getSession|supabase|firebase|clerk",
    ),
    hooks: js_regex(r"\buse[A-Z][a-zA-Z]+\b"),
    fetch: js_regex(r"fetch\([^)]{5,120}\)|axios\.[a-z]+\("),
    ai: js_regex(
        r"(?i-u:gemini|gpt-[34]|claude-?[a-z0-9]*|anthropic|openai|systemInstruction|systemPrompt)",
    ),
    baas: js_regex(
        r"[a-z0-9-]+\.supabase\.co|[a-z0-9-]+\.firebaseapp\.com|[a-z0-9-]+\.firebaseio\.com",
    ),
    function_calling: js_regex(
        r#"name:\s*["'][a-z_][a-z0-9_]*["']\s*,\s*description:\s*["'][^"']{5,}["']"#,
    ),
    rpc: js_regex(r#"this\.client\.(?:request|stream)\(["'][^"']+["']"#),
});

static URL_NOISE: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r#"(w3\.org|schema\.org|mozilla\.org|fonts\.googleapis|fonts\.gstatic|github\.com/[^"]*/issues|webpack|reactjs\.org|nextjs\.org)"#,
    )
});

pub fn pattern_for_category(c: Category) -> &'static Regex {
    let p = &*CATEGORY_PATTERNS;
    match c {
        Category::Api => &p.api,
        Category::Urls => &p.urls,
        Category::Env => &p.env,
        Category::Secrets => &p.secrets,
        Category::Auth => &p.auth,
        Category::Hooks => &p.hooks,
        Category::Fetch => &p.fetch,
        Category::Ai => &p.ai,
        Category::Baas => &p.baas,
        Category::FunctionCalling => &p.function_calling,
        Category::Rpc => &p.rpc,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GrepMatch {
    /// In UTF-16 code units, as the TypeScript reported it.
    pub offset: usize,
    #[serde(rename = "match")]
    pub text: String,
    pub snippet: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrepOptions {
    pub max: usize,
    pub before: usize,
    pub after: usize,
}

// Body size ceiling for grep: 10MB (of UTF-16 units, despite the name)
const GREP_BODY_MAX_BYTES: usize = 10 * 1024 * 1024;
// Most exec attempts per grep_body call (ReDoS defence in the TypeScript;
// here it bounds the work of one call)
const GREP_MAX_EXEC_ATTEMPTS: usize = 10_000;

fn too_large(body: &str) -> bool {
    // UTF-16 never needs more units than UTF-8 needs bytes.
    body.len() > GREP_BODY_MAX_BYTES && utf16_len(body) > GREP_BODY_MAX_BYTES
}

/// Dedup + snippet slicing shared by grep_body and grep_body_safe.
struct Assembler<'b, 'k> {
    body: &'b str,
    // The noise filter is tied to the urls category pattern itself, as the
    // TypeScript's `regex === CATEGORY_PATTERNS.urls` is: a caller's own
    // pattern that happens to look alike is not filtered.
    urls: bool,
    opts: GrepOptions,
    seen: HashSet<(&'k str, usize)>,
    out: Vec<GrepMatch>,
}

impl<'b, 'k> Assembler<'b, 'k> {
    fn new(body: &'b str, regex: &Regex, opts: &GrepOptions) -> Self {
        Self {
            body,
            urls: std::ptr::eq(regex, pattern_for_category(Category::Urls)),
            opts: *opts,
            seen: HashSet::new(),
            out: Vec::new(),
        }
    }

    fn full(&self) -> bool {
        self.out.len() >= self.opts.max
    }

    fn offer(&mut self, index: usize, byte_index: usize, text: &'k str) {
        if self.urls && URL_NOISE.is_match(text) {
            return;
        }
        if !self.seen.insert((text, index / 200)) {
            return;
        }
        let before = self.body.get(..byte_index).unwrap_or("");
        let start = utf16_suffix_start(before, self.opts.before);
        let end = byte_index + text.len();
        let end = end + utf16_prefix_len(self.body.get(end..).unwrap_or(""), self.opts.after);
        self.out.push(GrepMatch {
            offset: index,
            text: text.to_owned(),
            snippet: self.body.get(start..end).unwrap_or(text).to_owned(),
        });
    }
}

pub fn grep_body(body: &str, regex: &Regex, opts: &GrepOptions) -> Vec<GrepMatch> {
    // Over the size ceiling: empty result
    if too_large(body) {
        return Vec::new();
    }
    let mut assembler = Assembler::new(body, regex, opts);
    let mut offsets = Utf16Offsets::new(body);
    for (attempt, m) in exec_all(body, regex).enumerate() {
        if assembler.full() || attempt >= GREP_MAX_EXEC_ATTEMPTS {
            break;
        }
        if m.is_empty() {
            continue;
        }
        assembler.offer(offsets.at(m.start()), m.start(), m.as_str());
    }
    assembler.out
}

fn assemble_matches(
    body: &str,
    regex: &Regex,
    raw: &[RawMatch],
    opts: &GrepOptions,
) -> Vec<GrepMatch> {
    let mut assembler = Assembler::new(body, regex, opts);
    for r in raw {
        if assembler.full() {
            break;
        }
        assembler.offer(r.index, r.byte_index, &r.text);
    }
    assembler.out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrepError {
    Timeout(GrepTimeoutError),
    /// The search thread could not start or died without an answer.
    Worker(String),
}

impl fmt::Display for GrepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout(e) => e.fmt(f),
            Self::Worker(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for GrepError {}

/// Runs `job` on its own thread and waits at most `timeout_ms` for the answer.
/// On overrun the job is told to stop through the flag it was handed — a thread
/// cannot be terminated the way the TypeScript terminates its worker.
fn run_with_timeout<T: Send + 'static>(
    timeout_ms: u64,
    job: impl FnOnce(&AtomicBool) -> T + Send + 'static,
) -> Result<T, GrepError> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let flag = Arc::clone(&cancelled);
    thread::Builder::new()
        .name("grep".into())
        .spawn(move || {
            let _ = tx.send(job(&flag));
        })
        .map_err(|e| GrepError::Worker(e.to_string()))?;
    rx.recv_timeout(Duration::from_millis(timeout_ms))
        .map_err(|e| {
            cancelled.store(true, Ordering::Relaxed);
            match e {
                mpsc::RecvTimeoutError::Timeout => {
                    GrepError::Timeout(GrepTimeoutError { ms: timeout_ms })
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    GrepError::Worker("grep worker stopped without a result".into())
                }
            }
        })
}

/// Run `regex` against `body` on a thread with a hard timeout. `max` caps the
/// number of raw matches collected. Fails with `GrepError::Timeout` on overrun.
/// The TypeScript defaults `timeout_ms` to `GREP_TIMEOUT_MS`.
pub fn exec_regex_with_timeout(
    body: &str,
    regex: &Regex,
    max: usize,
    timeout_ms: u64,
) -> Result<Vec<RawMatch>, GrepError> {
    // The thread outlives this call on timeout, so it owns its inputs.
    let (body, regex) = (body.to_owned(), regex.clone());
    run_with_timeout(timeout_ms, move |cancelled| {
        exec_regex(&body, &regex, max, cancelled)
    })
}

/// Like grep_body, but runs the search on a thread with a wall-clock timeout so
/// an agent-supplied pattern can't freeze the caller. Fails with
/// `GrepError::Timeout` on overrun.
pub fn grep_body_safe(
    body: &str,
    regex: &Regex,
    opts: &GrepOptions,
) -> Result<Vec<GrepMatch>, GrepError> {
    if too_large(body) {
        return Ok(Vec::new());
    }
    // Collect a generous multiple of the caller's limit so dedup still fills it.
    let max = opts.max.saturating_mul(20).saturating_add(100);
    let raw = exec_regex_with_timeout(body, regex, max, GREP_TIMEOUT_MS)?;
    Ok(assemble_matches(body, regex, &raw, opts))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Bundler {
    pub name: &'static str,
    pub signature: &'static str,
}

struct BundlerPatterns {
    vite: Regex,
    turbopack: Regex,
    webpack: Regex,
    browserify_head: Regex,
    browserify: Regex,
    rollup_head: Regex,
    rollup: Regex,
}

static BUNDLER_PATTERNS: LazyLock<BundlerPatterns> = LazyLock::new(|| BundlerPatterns {
    vite: js_regex(r"__vite__|import\.meta\.(?:url|env|hot)"),
    turbopack: js_regex(r"(?i-u:\bturbopack\b)"),
    webpack: js_regex(r"__webpack_require__|webpackJsonp|webpackChunk"),
    browserify_head: js_regex(r"require=function\s*\("),
    browserify: js_regex(r"(?i-u:browserify)"),
    rollup_head: js_regex(r"Object\.defineProperty\(exports"),
    rollup: js_regex(r#"import\s*\{[^}]+\}\s*from\s*["']"#),
});

pub fn detect_bundler(body: &str) -> Bundler {
    let head = body.get(..utf16_prefix_len(body, 8000)).unwrap_or(body);
    let tail = body.get(utf16_suffix_start(body, 4000)..).unwrap_or(body);
    let both = format!("{head}{tail}");
    let p = &*BUNDLER_PATTERNS;
    let found = |name, signature| Bundler { name, signature };
    if p.vite.is_match(&both) {
        return found("vite", "import.meta / __vite__");
    }
    if p.turbopack.is_match(&both) {
        return found("turbopack", "turbopack marker");
    }
    if p.webpack.is_match(&both) {
        return found("webpack", "__webpack_require__ / webpackJsonp");
    }
    if p.browserify_head.is_match(head) && p.browserify.is_match(&both) {
        return found("browserify", "browserify runtime");
    }
    if p.rollup_head.is_match(head) && p.rollup.is_match(&both) {
        return found("rollup", "rollup ESM output");
    }
    found("unknown", "")
}

const WEBCRACK_TIMEOUT_MS: u64 = 30_000;
const WEBCRACK_MAX_OUTPUT: usize = 5 * 1024 * 1024;

/// Deobfuscates `body` with the `webcrack` CLI. The error is the message the
/// TypeScript rejected with.
pub fn run_webcrack(body: &str) -> Result<String, String> {
    // No file argument → webcrack reads from stdin (webcrack >=2 treats "-"
    // as a literal filename and errors out). Deobfuscated code goes to stdout.
    run_cli(
        Command::new("webcrack"),
        body,
        Duration::from_millis(WEBCRACK_TIMEOUT_MS),
        WEBCRACK_MAX_OUTPUT,
    )
}

/// Polls for the exit until `deadline`: std has no wait with a timeout.
fn wait_until(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait().ok()? {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn run_cli(
    mut command: Command,
    body: &str,
    timeout: Duration,
    max_output: usize,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // The TypeScript reads stderr and throws it away.
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            ErrorKind::NotFound => {
                "webcrack CLI not found on PATH. Install with: nvm use 22 && npm i -g webcrack"
                    .to_owned()
            }
            _ => e.to_string(),
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        let body = body.to_owned();
        // Fed from its own thread: a child that fills its stdout pipe before it
        // has read all of stdin would otherwise deadlock against this one.
        thread::spawn(move || {
            let _ = stdin.write_all(body.as_bytes());
        });
    }
    let (tx, rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        thread::spawn(move || {
            let mut out = Vec::new();
            // One byte past the cap is enough to know it was exceeded.
            let _ = stdout.take(max_output as u64 + 1).read_to_end(&mut out);
            let _ = tx.send(out);
        });
    }

    // None: no end of output within the time, or more output than allowed.
    let output = rx
        .recv_timeout(timeout)
        .ok()
        .filter(|out| out.len() <= max_output);
    let status = output
        .as_ref()
        .and_then(|_| wait_until(&mut child, deadline));
    let (Some(output), Some(status)) = (output, status) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "webcrack timed out or output exceeded {max_output} bytes"
        ));
    };
    if status.code() != Some(0) && output.is_empty() {
        // A child killed by a signal has no code; JavaScript prints that as null.
        let code = status.code().map_or("null".to_owned(), |c| c.to_string());
        return Err(format!("webcrack exited with code {code}"));
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(test)]
mod tests {
    use rever_core::traffic::RequestPatch;

    use super::*;

    fn re(pattern: &str) -> Regex {
        Regex::new(pattern).unwrap()
    }

    // ── grepBody ────────────────────────────────────────────────────────────

    const OPTS: GrepOptions = GrepOptions {
        max: 100,
        before: 0,
        after: 0,
    };

    #[test]
    fn 주변_문맥을_포함한_스니펫과_함께_매치를_찾는다() {
        let hits = grep_body(
            "abcTOKENdef",
            &re("TOKEN"),
            &GrepOptions {
                max: 10,
                before: 2,
                after: 2,
            },
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "TOKEN");
        assert_eq!(hits[0].offset, 3);
        assert_eq!(hits[0].snippet, "bcTOKENde");
    }

    #[test]
    fn 같은_200바이트_버킷_안의_동일한_매치는_하나로_합친다() {
        let body = format!("TOKEN{}TOKEN", " ".repeat(10));
        assert_eq!(grep_body(&body, &re("TOKEN"), &OPTS).len(), 1);
    }

    #[test]
    fn 다른_200바이트_버킷의_동일한_매치는_유지한다() {
        let body = format!("TOKEN{}TOKEN", " ".repeat(250));
        assert_eq!(grep_body(&body, &re("TOKEN"), &OPTS).len(), 2);
    }

    #[test]
    fn max_상한을_지킨다() {
        let body = ["TOKEN", "TOKEN", "TOKEN"].join(&" ".repeat(250));
        let opts = GrepOptions { max: 2, ..OPTS };
        assert_eq!(grep_body(&body, &re("TOKEN"), &opts).len(), 2);
    }

    #[test]
    fn 전역_플래그가_없는_정규식도_모든_매치를_찾는다() {
        // The TypeScript adds the `g` flag when the caller's regex lacks it;
        // a `regex::Regex` has no such flag and always searches the whole body.
        let body = format!("TOKEN{}TOKEN", " ".repeat(250));
        assert_eq!(grep_body(&body, &re("TOKEN"), &OPTS).len(), 2);
    }

    #[test]
    fn urls_카테고리에서는_노이즈_호스트를_걸러낸다() {
        let body = r#""https://w3.org/ns" and "https://api.example.com/v1/users""#;
        let hits = grep_body(body, pattern_for_category(Category::Urls), &OPTS);
        assert!(hits.iter().any(|h| h.text.contains("api.example.com")));
        assert!(!hits.iter().any(|h| h.text.contains("w3.org")));
    }

    // ── grepBodySafe ────────────────────────────────────────────────────────

    #[test]
    fn 일반_패턴에서는_grep_body와_같은_매치를_돌려준다() {
        let body = "abcTOKENdefTOKENghi";
        let opts = GrepOptions {
            max: 10,
            before: 2,
            after: 2,
        };
        let safe = grep_body_safe(body, &re("TOKEN"), &opts).unwrap();
        assert_eq!(safe, grep_body(body, &re("TOKEN"), &opts));
        assert_eq!(safe.len(), 1);
    }

    #[test]
    fn 제한_시간을_넘긴_검색은_타임아웃_오류가_되고_중단_신호를_받는다() {
        let (stopped, wait) = mpsc::channel();
        let result = run_with_timeout(20, move |cancelled| {
            while !cancelled.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(1));
            }
            let _ = stopped.send(());
        });
        let Err(GrepError::Timeout(timeout)) = result else {
            panic!("expected a timeout, got {result:?}");
        };
        assert_eq!(timeout, GrepTimeoutError { ms: 20 });
        assert_eq!(
            GrepError::Timeout(timeout).to_string(),
            "pattern timed out after 20ms (possible catastrophic backtracking)"
        );
        // The abandoned search saw the flag and stopped.
        assert!(wait.recv_timeout(Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn 파국적_역추적_패턴도_선형_시간에_끝난다() {
        // The TypeScript suite expects GrepTimeoutError here: V8 backtracks on
        // /(a+)+$/ and never returns. The regex crate does not backtrack, so the
        // same input answers at once — with no match, as the tail is "!".
        let body = format!("{}!", "a".repeat(40));
        let opts = GrepOptions {
            max: 5,
            before: 0,
            after: 0,
        };
        assert_eq!(grep_body_safe(&body, &re("(a+)+$"), &opts), Ok(vec![]));
    }

    // ── patternForCategory ──────────────────────────────────────────────────

    #[test]
    fn 카테고리에_맞는_정규식을_돌려준다() {
        assert!(pattern_for_category(Category::Api).is_match(r#"fetch("/api/users")"#));
    }

    // ── detectBundler ───────────────────────────────────────────────────────

    #[test]
    fn import_meta로_vite를_감지한다() {
        assert_eq!(detect_bundler("const x = import.meta.url").name, "vite");
    }

    #[test]
    fn webpack_require로_webpack을_감지한다() {
        assert_eq!(
            detect_bundler("function f(){ return __webpack_require__(0) }").name,
            "webpack"
        );
    }

    #[test]
    fn 마커로_turbopack을_감지한다() {
        assert_eq!(detect_bundler("/* turbopack runtime */").name, "turbopack");
    }

    #[test]
    fn 평범한_코드는_unknown을_돌려준다() {
        assert_eq!(detect_bundler("console.log(1 + 1)").name, "unknown");
    }

    // ── listScripts ─────────────────────────────────────────────────────────

    fn script_store() -> TrafficStore {
        let mut store = TrafficStore::default();
        let mut add = |id: &str, url: &str, kind: &str, body: Option<String>, base64: bool| {
            store.upsert(
                RequestPatch {
                    request_id: id.into(),
                    url: Some(url.into()),
                    host: Some("a".into()),
                    resource_type: Some(kind.into()),
                    response_body: body,
                    response_body_base64: base64.then_some(true),
                    ..RequestPatch::default()
                },
                0,
            );
        };
        add(
            "s1",
            "https://a/1.js",
            "Script",
            Some("a".repeat(100)),
            false,
        );
        add(
            "s2",
            "https://a/2.js",
            "Script",
            Some("b".repeat(300)),
            false,
        );
        add(
            "s3",
            "https://a/3.js",
            "Script",
            Some("c".repeat(50)),
            false,
        );
        // excluded: base64-encoded body
        add(
            "s4",
            "https://a/4.js",
            "Script",
            Some("x".repeat(400)),
            true,
        );
        // excluded: not a script
        add("s5", "https://a/data", "XHR", Some("d".repeat(500)), false);
        // excluded: no body
        add("s6", "https://a/6.js", "Script", None, false);
        store
    }

    fn ids(rows: &[&StoredRequest]) -> Vec<String> {
        rows.iter().map(|r| r.request_id.clone()).collect()
    }

    #[test]
    fn 스크립트_본문을_크기순으로_돌려주고_base64_비스크립트_본문_없는_것은_뺀다() {
        let store = script_store();
        assert_eq!(
            ids(&list_scripts(&store, &ScriptFilter::default())),
            ["s2", "s1", "s3"]
        );
    }

    #[test]
    fn min_size_필터를_적용한다() {
        let store = script_store();
        let filter = ScriptFilter {
            min_size: Some(100),
            ..ScriptFilter::default()
        };
        assert_eq!(ids(&list_scripts(&store, &filter)), ["s2", "s1"]);
    }

    // ── not in the TypeScript suite ─────────────────────────────────────────

    #[test]
    fn limit과_host로_스크립트_목록을_좁힌다() {
        let store = script_store();
        let limited = ScriptFilter {
            limit: Some(1),
            ..ScriptFilter::default()
        };
        assert_eq!(ids(&list_scripts(&store, &limited)), ["s2"]);
        let other_host = ScriptFilter {
            host: Some("b".into()),
            ..ScriptFilter::default()
        };
        assert!(list_scripts(&store, &other_host).is_empty());
    }

    #[test]
    fn 스크립트_크기는_encoded_data_length가_있으면_그것으로_잰다() {
        let mut store = TrafficStore::default();
        store.upsert(
            RequestPatch {
                request_id: "gz".into(),
                url: Some("https://a/gz.js".into()),
                resource_type: Some("Script".into()),
                response_body: Some("a".repeat(500)),
                encoded_data_length: Some(80),
                ..RequestPatch::default()
            },
            0,
        );
        let filter = ScriptFilter {
            min_size: Some(100),
            ..ScriptFilter::default()
        };
        assert!(list_scripts(&store, &filter).is_empty());
    }

    #[test]
    fn 오프셋과_스니펫_폭은_utf16_단위로_센다() {
        // "가" is one UTF-16 unit (three bytes), "😀" two units (four bytes).
        let hits = grep_body(
            "가나😀TOKEN😀다라",
            &re("TOKEN"),
            &GrepOptions {
                max: 10,
                before: 3,
                after: 3,
            },
        );
        assert_eq!(hits[0].offset, 4);
        assert_eq!(hits[0].snippet, "나😀TOKEN😀다");
        // A width that would split a surrogate pair leaves the pair out.
        let narrow = grep_body(
            "😀TOKEN😀",
            &re("TOKEN"),
            &GrepOptions {
                max: 10,
                before: 1,
                after: 1,
            },
        );
        assert_eq!(narrow[0].snippet, "TOKEN");
    }

    #[test]
    fn 버킷은_utf16_오프셋으로_나눈다() {
        // 100 Hangul characters are 300 bytes but 100 UTF-16 units: same bucket.
        let body = format!("TOKEN{}TOKEN", "가".repeat(100));
        assert_eq!(grep_body(&body, &re("TOKEN"), &OPTS).len(), 1);
    }

    #[test]
    fn 빈_문자열에_매치하는_패턴도_끝난다() {
        let hits = grep_body("abxxcx", &re("x*"), &OPTS);
        assert_eq!(
            hits.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(),
            ["xx", "x"]
        );
    }

    #[test]
    fn exec_시도는_만_번에서_멈춘다() {
        // One hit survives dedup per 200-unit bucket; 10,000 attempts cover 50 buckets.
        let body = "x".repeat(30_000);
        let opts = GrepOptions { max: 1000, ..OPTS };
        assert_eq!(grep_body(&body, &re("x"), &opts).len(), 50);
        // The safe variant caps raw matches instead: max * 20 + 100.
        let small = GrepOptions { max: 5, ..OPTS };
        assert_eq!(grep_body_safe(&body, &re("x"), &small).unwrap().len(), 1);
    }

    #[test]
    fn 본문이_10mb를_넘으면_빈_결과를_돌려준다() {
        let body = format!("TOKEN{}", " ".repeat(GREP_BODY_MAX_BYTES));
        assert!(grep_body(&body, &re("TOKEN"), &OPTS).is_empty());
        assert_eq!(grep_body_safe(&body, &re("TOKEN"), &OPTS), Ok(vec![]));
        // The ceiling counts UTF-16 units: 4MB of Hangul is 12MB of bytes.
        let hangul = format!("TOKEN{}", "가".repeat(4 * 1024 * 1024));
        assert_eq!(grep_body(&hangul, &re("TOKEN"), &OPTS).len(), 1);
    }

    #[test]
    fn 호출자가_만든_같은_모양의_url_패턴에는_노이즈_필터를_걸지_않는다() {
        let body = r#""https://w3.org/ns""#;
        let own = re(pattern_for_category(Category::Urls).as_str());
        assert_eq!(grep_body(body, &own, &OPTS).len(), 1);
        assert!(
            grep_body_safe(body, pattern_for_category(Category::Urls), &OPTS)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn 카테고리마다_대표_문자열을_찾는다() {
        let cases = [
            (Category::Api, r#"x="/api/users""#, r#""/api/users""#),
            (
                Category::Api,
                "`https://h/v2/items`",
                "`https://h/v2/items`",
            ),
            (
                Category::Urls,
                "see https://a.io/x?y=1 now",
                "https://a.io/x?y=1",
            ),
            (Category::Env, "process.env.API_URL;", "process.env.API_URL"),
            (Category::Env, "a NEXT_PUBLIC_KEY b", "NEXT_PUBLIC_KEY"),
            (
                Category::Secrets,
                "k=eyJhbGciOiJIUzI1NiIsInR5cCI6 ",
                "eyJhbGciOiJIUzI1NiIsInR5cCI6",
            ),
            (Category::Auth, "await getSession()", "getSession"),
            (Category::Hooks, "const s = useState(0)", "useState"),
            (
                Category::Fetch,
                r#"fetch("/x/y", o)"#,
                r#"fetch("/x/y", o)"#,
            ),
            (Category::Fetch, "axios.post(u)", "axios.post("),
            (Category::Ai, "model: 'Claude-3x'", "Claude-3x"),
            (
                Category::Baas,
                "https://abc.supabase.co/rest",
                "abc.supabase.co",
            ),
            (
                Category::FunctionCalling,
                r#"{name: "get_weather",description:"Returns weather"}"#,
                r#"name: "get_weather",description:"Returns weather""#,
            ),
            (
                Category::Rpc,
                r#"this.client.request("users.list", {})"#,
                r#"this.client.request("users.list""#,
            ),
        ];
        for (category, body, expected) in cases {
            let found = pattern_for_category(category)
                .find(body)
                .map(|m| m.as_str());
            assert_eq!(found, Some(expected), "{category:?}");
        }
    }

    #[test]
    fn hooks_패턴의_단어_경계는_아스키_기준이다() {
        // JavaScript's \b sees a boundary between Hangul and "use"; Unicode \b would not.
        let hooks = pattern_for_category(Category::Hooks);
        assert_eq!(
            hooks.find("가useState나").map(|m| m.as_str()),
            Some("useState")
        );
        assert!(!hooks.is_match("reuseState"));
    }

    #[test]
    fn 패턴의_공백과_대소문자_무시는_자바스크립트_기준이다() {
        // Checked against the TypeScript: \s takes NBSP, U+2028, U+3000 and the
        // BOM but not NEL, and the `i` flag does not fold "ſ" or the Kelvin sign.
        let calls = pattern_for_category(Category::FunctionCalling);
        let spaced = "name:\u{a0}\"c\"\u{2028},\u{feff}description:\u{3000}\"nbsp etc\"";
        assert_eq!(calls.find(spaced).map(|m| m.as_str()), Some(spaced));
        assert!(!calls.is_match("name:\u{85}\"d\",description:\"nel here\""));
        assert!(!calls.is_match("name:\u{200b}\"d\",description:\"zero width\""));

        let ai = pattern_for_category(Category::Ai);
        assert!(!ai.is_match("\u{17f}ystemPrompt"));
        assert_eq!(
            ai.find("SYSTEMPROMPT").map(|m| m.as_str()),
            Some("SYSTEMPROMPT")
        );
        assert_eq!(
            ai.find("claude-\u{212a}9").map(|m| m.as_str()),
            Some("claude-")
        );
        assert_eq!(
            detect_bundler("require=function(){} brow\u{17f}erify").name,
            "unknown"
        );
        assert_eq!(detect_bundler("TURBOPAC\u{212a}").name, "unknown");
        // NBSP between `function` and `(` is whitespace to JavaScript.
        assert_eq!(
            detect_bundler("require=function\u{a0}(e){} BROWSERIFY").name,
            "browserify"
        );
    }

    #[test]
    fn 타임아웃을_건_검색은_원시_매치를_max까지_돌려준다() {
        let found = exec_regex_with_timeout("a a a a", &re("a"), 3, GREP_TIMEOUT_MS).unwrap();
        assert_eq!(found.iter().map(|m| m.index).collect::<Vec<_>>(), [0, 2, 4]);
    }

    #[test]
    fn 카테고리_이름은_kebab_case로_직렬화한다() {
        assert_eq!(
            serde_json::to_string(&Category::FunctionCalling).unwrap(),
            r#""function-calling""#
        );
        assert_eq!(
            serde_json::from_str::<Category>(r#""urls""#).unwrap(),
            Category::Urls
        );
    }

    #[test]
    fn 매치는_match_필드로_직렬화한다() {
        let hits = grep_body("abcTOKEN", &re("TOKEN"), &OPTS);
        assert_eq!(
            serde_json::to_string(&hits[0]).unwrap(),
            r#"{"offset":3,"match":"TOKEN","snippet":"TOKEN"}"#
        );
    }

    #[test]
    fn browserify와_rollup을_감지한다() {
        let browserify = detect_bundler("require=function (e,n){} /* Browserify */");
        assert_eq!(
            (browserify.name, browserify.signature),
            ("browserify", "browserify runtime")
        );
        let rollup =
            detect_bundler("Object.defineProperty(exports, '__esModule'); import { a } from 'b'");
        assert_eq!(rollup.name, "rollup");
        // Without the runtime marker in the head, neither applies.
        assert_eq!(detect_bundler("// browserify").name, "unknown");
        assert_eq!(detect_bundler("").signature, "");
    }

    #[test]
    fn 번들러_표식은_앞_8000자와_뒤_4000자에서만_찾는다() {
        let filler = "x".repeat(20_000);
        assert_eq!(
            detect_bundler(&format!("{filler}__webpack_require__{filler}")).name,
            "unknown"
        );
        assert_eq!(
            detect_bundler(&format!("{filler}webpackChunk")).name,
            "webpack"
        );
        // vite is checked before webpack.
        assert_eq!(
            detect_bundler(&format!("__webpack_require__{filler}__vite__")).name,
            "vite"
        );
        // "turbopacked" has no word boundary after the marker.
        assert_eq!(detect_bundler("turbopacked TURBOPACK").name, "turbopack");
        assert_eq!(detect_bundler("turbopacked").name, "unknown");
    }

    // ── webcrack ────────────────────────────────────────────────────────────

    const SECOND: Duration = Duration::from_secs(10);

    #[test]
    fn webcrack이_없으면_설치_방법을_알린다() {
        let missing = Command::new("rever-analysis-no-such-webcrack-binary");
        assert_eq!(
            run_cli(missing, "var a = 1", SECOND, WEBCRACK_MAX_OUTPUT),
            Err(
                "webcrack CLI not found on PATH. Install with: nvm use 22 && npm i -g webcrack"
                    .to_owned()
            )
        );
    }

    #[test]
    fn webcrack_제한값은_30초와_5mb다() {
        assert_eq!(WEBCRACK_TIMEOUT_MS, 30_000);
        assert_eq!(WEBCRACK_MAX_OUTPUT, 5 * 1024 * 1024);
        assert_eq!(GREP_TIMEOUT_MS, 5000);
    }

    #[cfg(unix)]
    fn sh(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[cfg(unix)]
    #[test]
    fn 표준입력으로_본문을_주고_표준출력을_돌려받는다() {
        // Larger than a pipe buffer, so both directions must be pumped at once.
        let body = "var 가 = 1;\n".repeat(50_000);
        assert_eq!(
            run_cli(Command::new("cat"), &body, SECOND, WEBCRACK_MAX_OUTPUT),
            Ok(body)
        );
    }

    #[cfg(unix)]
    #[test]
    fn 출력_없이_실패하면_종료_코드를_알린다() {
        assert_eq!(
            run_cli(sh("cat >/dev/null; exit 3"), "x", SECOND, 1024),
            Err("webcrack exited with code 3".to_owned())
        );
        // Output with a failing exit code is still returned, as in the TypeScript.
        assert_eq!(
            run_cli(sh("cat; exit 3"), "partial", SECOND, 1024),
            Ok("partial".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn 출력이_상한을_넘으면_죽이고_오류를_낸다() {
        assert_eq!(
            run_cli(Command::new("yes"), "", SECOND, 1024),
            Err("webcrack timed out or output exceeded 1024 bytes".to_owned())
        );
        // Exactly at the cap is still allowed.
        assert_eq!(
            run_cli(Command::new("cat"), &"a".repeat(1024), SECOND, 1024).map(|out| out.len()),
            Ok(1024)
        );
    }

    #[cfg(unix)]
    #[test]
    fn 제한_시간을_넘기면_죽이고_오류를_낸다() {
        let started = Instant::now();
        assert_eq!(
            run_cli(sh("sleep 30"), "", Duration::from_millis(100), 1024),
            Err("webcrack timed out or output exceeded 1024 bytes".to_owned())
        );
        // A child that closes stdout but keeps running is cut off at the deadline too.
        assert_eq!(
            run_cli(
                sh("exec >&-; sleep 30"),
                "",
                Duration::from_millis(100),
                1024
            ),
            Err("webcrack timed out or output exceeded 1024 bytes".to_owned())
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
