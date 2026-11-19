//! Port of `src/main/mcp/wasm-analysis.ts` — helpers behind the wasm tools:
//! the binary-body encoding decision for re-fetched responses, access to
//! captured `.wasm` bodies, disassembly, a strings scan and the export ↔ JS
//! cross-reference.
//!
//! The TypeScript leans on the `wabt` npm package. Here the WAT baseline is
//! `wasmprinter` and the section summary is read with `wasmparser`, both
//! in-process and always available; only the C-like decompile and the C source
//! shell out, to wabt's `wasm-decompile` / `wasm2c` when they are on PATH.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write as _};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use encoding_rs::Encoding;
use regex::Regex;
use rever_analysis::script_analysis::{GrepOptions, ScriptFilter, grep_body, list_scripts};
use rever_core::traffic::{ListFilter, StoredRequest, TrafficStore};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde::Serialize;
use wasmparser::{
    CompositeInnerType, ConstExpr, DataKind, ElementItems, ElementKind, ExternalKind, KnownCustom,
    Name, Operator, Parser, Payload, TypeRef,
};

use crate::js::{JS_SPACE, node_base64_decode, utf16_len};
use crate::js_regex::js_regex;

/// What a missing `wasm-decompile` / `wasm2c` reports. The TypeScript's hint
/// installs the npm package; this port runs the binaries from PATH.
pub const WABT_INSTALL_HINT: &str = "wabt not installed. Install with: brew install wabt (macOS) or apt install wabt (Debian/Ubuntu), so that wasm-decompile and wasm2c are on PATH";

// ── refetch body encoding (chrome-cdp.ts fallback path) ──────────────────────

// Binary MIME types whose bytes are mangled by a utf8 decode. `.wasm` is the
// reason this exists: a cache- or service-worker-delivered module re-fetched by
// refetchBody() must keep its `\0asm` magic and every non-utf8 byte intact.
const BINARY_MIME_PREFIXES: [&str; 6] = [
    "application/wasm",
    "application/octet-stream",
    "image/",
    "video/",
    "audio/",
    "font/",
];

static CHARSET: LazyLock<Regex> = LazyLock::new(|| {
    // `-u`: JavaScript's `i` flag does not fold "ſ" onto "s".
    Regex::new(&format!(
        r#"(?i-u:charset){JS_SPACE}*={JS_SPACE}*["']?([^;"'[{JS_SPACE}]]+)"#
    ))
    .expect("the pattern is a constant")
});

/// The charset from a Content-Type header (`text/html; charset=euc-kr`).
pub fn charset_from_content_type(content_type: Option<&str>) -> Option<String> {
    let found = CHARSET.captures(content_type.unwrap_or(""))?;
    Some(found.get(1)?.as_str().to_lowercase())
}

/// Decode bytes with the charset the response declared. Korean sites still
/// serve euc-kr/cp949, and decoding those as utf8 yields mojibake — the whole
/// body reads as `\uFFFD`. `encoding_rs` knows the labels TextDecoder does; an
/// unknown label makes TextDecoder throw, so fall back to utf8.
fn decode_text(buf: &[u8], charset: Option<&str>) -> String {
    let label = charset.unwrap_or("utf-8");
    // Node's TextDecoder (ICU) has no x-user-defined decoder and throws for it.
    let known = Encoding::for_label_no_replacement(label.as_bytes())
        .filter(|encoding| *encoding != encoding_rs::X_USER_DEFINED);
    let text = match known {
        Some(encoding) => encoding.decode_with_bom_removal(buf).0.into_owned(),
        None => String::from_utf8_lossy(buf).into_owned(),
    };
    // A utf8 BOM survives decoding as a zero-width U+FEFF that breaks JSON.parse
    // and shows up as a stray glyph in the body panes.
    match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefetchedBody {
    pub response_body: String,
    pub response_body_base64: bool,
}

/// Decide how to store a re-fetched response body. Binary MIME types are
/// base64-encoded (lossless); everything else is decoded as text using the
/// declared charset.
pub fn encode_refetched_body(
    buf: &[u8],
    mime: Option<&str>,
    charset: Option<&str>,
) -> RefetchedBody {
    let is_binary =
        mime.is_some_and(|mime| BINARY_MIME_PREFIXES.iter().any(|p| mime.starts_with(p)));
    if is_binary {
        RefetchedBody {
            response_body: STANDARD.encode(buf),
            response_body_base64: true,
        }
    } else {
        RefetchedBody {
            response_body: decode_text(buf, charset),
            response_body_base64: false,
        }
    }
}

// ── captured-body access + listing ───────────────────────────────────────────

static WASM_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u)\.wasm(\?|$)").expect("the pattern is a constant"));

pub fn is_wasm(r: &StoredRequest) -> bool {
    r.mime_type.as_deref() == Some("application/wasm") || WASM_URL.is_match(&r.url)
}

/// Captured WASM modules (application/wasm body or .wasm URL), largest first.
pub fn list_wasm<'a>(store: &'a TrafficStore, host: Option<&str>) -> Vec<&'a StoredRequest> {
    let mut rows: Vec<&StoredRequest> = store
        .list(&ListFilter {
            host: host.map(str::to_owned),
            limit: Some(200),
            ..ListFilter::default()
        })
        .into_iter()
        .filter(|r| is_wasm(r))
        .collect();
    // Stable, like Array.prototype.sort: equal sizes stay newest first.
    rows.sort_by_key(|r| Reverse(body_byte_length(r)));
    rows
}

/// Size of the captured body: decoded bytes for a base64 body, UTF-16 units
/// (the TypeScript's `length`) for a text one.
pub(crate) fn body_byte_length(r: &StoredRequest) -> usize {
    let Some(body) = r.response_body.as_deref() else {
        return 0;
    };
    if r.response_body_base64 == Some(true) {
        // Math.floor(length * 0.75)
        utf16_len(body) * 3 / 4
    } else {
        utf16_len(body)
    }
}

/// Decode a captured WASM body to bytes, or return the error text.
pub fn get_wasm_buffer(store: &TrafficStore, request_id: &str) -> Result<Vec<u8>, String> {
    let Some(entry) = store.get(request_id) else {
        return Err(format!("unknown requestId: {request_id}"));
    };
    let Some(body) = entry.response_body.as_deref().filter(|b| !b.is_empty()) else {
        return Err(
            "that request has no captured body (open the page so it loads, then try list_wasm)"
                .into(),
        );
    };
    Ok(if entry.response_body_base64 == Some(true) {
        node_base64_decode(body)
    } else {
        // Buffer.from(body, 'binary'): the low byte of every UTF-16 unit.
        body.encode_utf16().map(|unit| unit as u8).collect()
    })
}

// ── disassembly ──────────────────────────────────────────────────────────────

/// WAT baseline — in-process, no subprocess, no temp file. Debug names from
/// the name section are used, as with wabt's `readDebugNames`.
pub fn wasm_to_wat(wasm: &[u8]) -> Result<String, String> {
    wasmprinter::print_bytes(wasm).map_err(|e| format!("{e:#}"))
}

const WASM_TIMEOUT_MS: u64 = 30_000;
const WASM_MAX_OUTPUT: usize = 5 * 1024 * 1024;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Shell out to a wabt binary on PATH (wasm-decompile, wasm2c) — these produce
/// output the in-process crates can't. Writes a temp file (wabt reads a path,
/// not stdin), passes `extra_args` before it, and always cleans up. Blocks for
/// up to 30 seconds; callers degrade on error.
pub fn run_wabt_bin(tool: &str, wasm: &[u8], extra_args: &[&str]) -> Result<String, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = std::env::temp_dir().join(format!(
        "rever-{}-{nanos}-{}.wasm",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    // create_new: never write through a file someone else put at that path.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut file| file.write_all(wasm))
        .map_err(|e| e.to_string())?;
    let result = run_bin(
        tool,
        extra_args,
        &tmp,
        Duration::from_millis(WASM_TIMEOUT_MS),
        WASM_MAX_OUTPUT,
    );
    let _ = std::fs::remove_file(&tmp);
    result
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

fn run_bin(
    tool: &str,
    extra_args: &[&str],
    input: &Path,
    timeout: Duration,
    max_output: usize,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let mut child = Command::new(tool)
        .args(extra_args)
        .arg(input)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // The TypeScript reads stderr and throws it away.
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            ErrorKind::NotFound => WABT_INSTALL_HINT.to_owned(),
            _ => e.to_string(),
        })?;
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
            "{tool} timed out or output exceeded {max_output} bytes"
        ));
    };
    if status.code() != Some(0) && output.is_empty() {
        // A child killed by a signal has no code; JavaScript prints that as null.
        let code = status
            .code()
            .map_or_else(|| "null".to_owned(), |code| code.to_string());
        return Err(format!("{tool} exited with code {code}"));
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

/// C-like decompilation (`wasm-decompile`).
pub fn wasm_decompile_full(wasm: &[u8]) -> Result<String, String> {
    run_wabt_bin("wasm-decompile", wasm, &[])
}

/// Full C source (`wasm2c`) — the most readable output for complex routines.
pub fn wasm_to_c(wasm: &[u8]) -> Result<String, String> {
    run_wabt_bin("wasm2c", wasm, &[])
}

/// What `init` expressions the summary spells out; anything else is left blank.
fn init_text(expr: &ConstExpr<'_>) -> String {
    match expr.get_operators_reader().read() {
        Ok(Operator::I32Const { value }) => format!(" - init i32={value}"),
        Ok(Operator::I64Const { value }) => format!(" - init i64={value}"),
        Ok(Operator::GlobalGet { global_index }) => format!(" - init global={global_index}"),
        _ => String::new(),
    }
}

fn limits_text(initial: u64, maximum: Option<u64>) -> String {
    match maximum {
        Some(max) => format!("initial={initial} max={max}"),
        None => format!("initial={initial}"),
    }
}

fn type_ref_text(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Func(sig) | TypeRef::FuncExact(sig) => format!("sig={sig}"),
        TypeRef::Table(table) => format!(
            "type={} {}",
            table.element_type,
            limits_text(table.initial, table.maximum)
        ),
        TypeRef::Memory(memory) => {
            format!("pages: {}", limits_text(memory.initial, memory.maximum))
        }
        TypeRef::Global(global) => format!(
            "{} mutable={}",
            global.content_type,
            u8::from(global.mutable)
        ),
        TypeRef::Tag(tag) => format!("sig={}", tag.func_type_idx),
    }
}

/// Function names for the summary, as wasm-objdump picks them: the name
/// section first, else an export name, else `module.field` of an import.
fn function_names(wasm: &[u8]) -> HashMap<u32, String> {
    let mut imported = HashMap::new();
    let mut exported = HashMap::new();
    let mut named = HashMap::new();
    let mut next_function = 0;
    for payload in Parser::new(0).parse_all(wasm).map_while(Result::ok) {
        match payload {
            Payload::ImportSection(reader) => {
                for import in reader.into_imports().map_while(Result::ok) {
                    if matches!(import.ty, TypeRef::Func(_) | TypeRef::FuncExact(_)) {
                        imported
                            .insert(next_function, format!("{}.{}", import.module, import.name));
                        next_function += 1;
                    }
                }
            }
            Payload::ExportSection(reader) => {
                for export in reader.into_iter().map_while(Result::ok) {
                    if matches!(export.kind, ExternalKind::Func | ExternalKind::FuncExact) {
                        exported
                            .entry(export.index)
                            .or_insert_with(|| export.name.to_owned());
                    }
                }
            }
            Payload::CustomSection(reader) => {
                let KnownCustom::Name(names) = reader.as_known() else {
                    continue;
                };
                for name in names.into_iter().map_while(Result::ok) {
                    let Name::Function(map) = name else {
                        continue;
                    };
                    for naming in map.into_iter().map_while(Result::ok) {
                        named.insert(naming.index, naming.name.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    imported.extend(exported);
    imported.extend(named);
    imported
}

/// Module summary + symbol names, laid out like `wasm-objdump -x`: types,
/// imports, exports and — with a name section — the real function names. Read
/// in-process, so unlike the TypeScript it needs no wabt binary; data segment
/// hex dumps are left out (grep_wasm reads those).
pub fn wasm_objdump(wasm: &[u8]) -> Result<String, String> {
    let names = function_names(wasm);
    let name_of = |index: u32| {
        names
            .get(&index)
            .map_or_else(String::new, |name| format!(" <{name}>"))
    };
    let mut out = String::new();
    // Next free index of each index space: imports come first.
    let (mut functions, mut tables, mut memories, mut globals, mut tags) = (0, 0, 0, 0, 0);
    let mut code_index = 0;
    for payload in Parser::new(0).parse_all(wasm) {
        // Writing to a String cannot fail, hence the ignored results below.
        match payload.map_err(|e| e.to_string())? {
            Payload::Version { num, .. } => {
                let _ = write!(
                    out,
                    "\nmodule.wasm:\tfile format wasm {num:#x}\n\nSection Details:\n\n"
                );
            }
            Payload::TypeSection(reader) => {
                let mut lines = Vec::new();
                for group in reader {
                    for sub in group.map_err(|e| e.to_string())?.types() {
                        let index = lines.len();
                        lines.push(match &sub.composite_type.inner {
                            CompositeInnerType::Func(func) => {
                                let list = |types: &[wasmparser::ValType]| {
                                    let texts: Vec<String> =
                                        types.iter().map(ToString::to_string).collect();
                                    texts.join(", ")
                                };
                                let results = match func.results() {
                                    [] => "nil".to_owned(),
                                    [one] => one.to_string(),
                                    many => format!("({})", list(many)),
                                };
                                format!(" - type[{index}] ({}) -> {results}", list(func.params()))
                            }
                            other => format!(" - type[{index}] {other}"),
                        });
                    }
                }
                let _ = writeln!(out, "Type[{}]:", lines.len());
                for line in lines {
                    let _ = writeln!(out, "{line}");
                }
            }
            Payload::ImportSection(reader) => {
                let mut lines = Vec::new();
                for import in reader.into_imports() {
                    let import = import.map_err(|e| e.to_string())?;
                    let (kind, counter) = match import.ty {
                        TypeRef::Func(_) | TypeRef::FuncExact(_) => ("func", &mut functions),
                        TypeRef::Table(_) => ("table", &mut tables),
                        TypeRef::Memory(_) => ("memory", &mut memories),
                        TypeRef::Global(_) => ("global", &mut globals),
                        TypeRef::Tag(_) => ("tag", &mut tags),
                    };
                    let name = match kind {
                        "func" => name_of(*counter),
                        _ => String::new(),
                    };
                    lines.push(format!(
                        " - {kind}[{counter}] {}{name} <- {}.{}",
                        type_ref_text(&import.ty),
                        import.module,
                        import.name
                    ));
                    *counter += 1;
                }
                let _ = writeln!(out, "Import[{}]:", lines.len());
                for line in lines {
                    let _ = writeln!(out, "{line}");
                }
            }
            Payload::FunctionSection(reader) => {
                let _ = writeln!(out, "Function[{}]:", reader.count());
                for (offset, sig) in reader.into_iter().enumerate() {
                    let index = functions.saturating_add(offset as u32);
                    let sig = sig.map_err(|e| e.to_string())?;
                    let _ = writeln!(out, " - func[{index}] sig={sig}{}", name_of(index));
                }
            }
            Payload::TableSection(reader) => {
                let _ = writeln!(out, "Table[{}]:", reader.count());
                for (offset, table) in reader.into_iter().enumerate() {
                    let ty = table.map_err(|e| e.to_string())?.ty;
                    let _ = writeln!(
                        out,
                        " - table[{}] {}",
                        tables.saturating_add(offset as u32),
                        type_ref_text(&TypeRef::Table(ty))
                    );
                }
            }
            Payload::MemorySection(reader) => {
                let _ = writeln!(out, "Memory[{}]:", reader.count());
                for (offset, memory) in reader.into_iter().enumerate() {
                    let ty = memory.map_err(|e| e.to_string())?;
                    let _ = writeln!(
                        out,
                        " - memory[{}] {}",
                        memories.saturating_add(offset as u32),
                        type_ref_text(&TypeRef::Memory(ty))
                    );
                }
            }
            Payload::GlobalSection(reader) => {
                let _ = writeln!(out, "Global[{}]:", reader.count());
                for (offset, global) in reader.into_iter().enumerate() {
                    let global = global.map_err(|e| e.to_string())?;
                    let _ = writeln!(
                        out,
                        " - global[{}] {}{}",
                        globals.saturating_add(offset as u32),
                        type_ref_text(&TypeRef::Global(global.ty)),
                        init_text(&global.init_expr)
                    );
                }
            }
            Payload::ExportSection(reader) => {
                let _ = writeln!(out, "Export[{}]:", reader.count());
                for export in reader {
                    let export = export.map_err(|e| e.to_string())?;
                    let (kind, name) = match export.kind {
                        ExternalKind::Func | ExternalKind::FuncExact => {
                            ("func", name_of(export.index))
                        }
                        ExternalKind::Table => ("table", String::new()),
                        ExternalKind::Memory => ("memory", String::new()),
                        ExternalKind::Global => ("global", String::new()),
                        ExternalKind::Tag => ("tag", String::new()),
                    };
                    let _ = writeln!(
                        out,
                        " - {kind}[{}]{name} -> \"{}\"",
                        export.index, export.name
                    );
                }
            }
            Payload::StartSection { func, .. } => {
                let _ = writeln!(out, "Start:\n - start function: {func}");
            }
            Payload::ElementSection(reader) => {
                let _ = writeln!(out, "Elem[{}]:", reader.count());
                for (index, element) in reader.into_iter().enumerate() {
                    let element = element.map_err(|e| e.to_string())?;
                    let count = match &element.items {
                        ElementItems::Functions(items) => items.count(),
                        ElementItems::Expressions(_, items) => items.count(),
                    };
                    let place = match &element.kind {
                        ElementKind::Passive => "passive".to_owned(),
                        ElementKind::Declared => "declared".to_owned(),
                        ElementKind::Active { table_index, .. } => {
                            format!("table={}", table_index.unwrap_or(0))
                        }
                    };
                    let init = match &element.kind {
                        ElementKind::Active { offset_expr, .. } => init_text(offset_expr),
                        _ => String::new(),
                    };
                    let _ = writeln!(out, " - segment[{index}] {place} count={count}{init}");
                }
            }
            Payload::DataCountSection { count, .. } => {
                let _ = writeln!(out, "DataCount:\n - data count: {count}");
            }
            Payload::CodeSectionStart { count, .. } => {
                let _ = writeln!(out, "Code[{count}]:");
            }
            Payload::CodeSectionEntry(body) => {
                let index = functions.saturating_add(code_index);
                code_index += 1;
                let range = body.range();
                let _ = writeln!(
                    out,
                    " - func[{index}] size={}{}",
                    range.end.saturating_sub(range.start),
                    name_of(index)
                );
            }
            Payload::DataSection(reader) => {
                let _ = writeln!(out, "Data[{}]:", reader.count());
                for (index, data) in reader.into_iter().enumerate() {
                    let data = data.map_err(|e| e.to_string())?;
                    let size = data.data.len();
                    let _ = match &data.kind {
                        DataKind::Passive => {
                            writeln!(out, " - segment[{index}] passive size={size}")
                        }
                        DataKind::Active {
                            memory_index,
                            offset_expr,
                        } => writeln!(
                            out,
                            " - segment[{index}] memory={memory_index} size={size}{}",
                            init_text(offset_expr)
                        ),
                    };
                }
            }
            Payload::CustomSection(reader) => {
                let _ = writeln!(out, "Custom:\n - name: \"{}\"", reader.name());
                let KnownCustom::Name(subsections) = reader.as_known() else {
                    continue;
                };
                for subsection in subsections.into_iter().map_while(Result::ok) {
                    let Name::Function(map) = subsection else {
                        continue;
                    };
                    for naming in map.into_iter().map_while(Result::ok) {
                        let _ = writeln!(out, " - func[{}] <{}>", naming.index, naming.name);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmFormat {
    Wat,
    Decompile,
    C,
}

fn wasm_bytes(traffic: &Mutex<TrafficStore>, request_id: &str) -> Result<Vec<u8>, String> {
    let store = traffic.lock().unwrap_or_else(|p| p.into_inner());
    get_wasm_buffer(&store, request_id)
}

/// Render flow behind the tool. Returns an MCP `ok`/`err` result (kept out of
/// the tool so it is directly unit-testable). Non-WAT formats shell out and
/// degrade to WAT if that path is unavailable; WAT (in-process) is the
/// guaranteed baseline.
pub async fn decompile_request(
    traffic: &Mutex<TrafficStore>,
    request_id: &str,
    format: WasmFormat,
) -> ToolResult {
    let bytes = match wasm_bytes(traffic, request_id) {
        Ok(bytes) => bytes,
        Err(message) => return err(message),
    };
    if format != WasmFormat::Wat {
        let input = bytes.clone();
        let rendered = tokio::task::spawn_blocking(move || match format {
            WasmFormat::C => wasm_to_c(&input),
            _ => wasm_decompile_full(&input),
        })
        .await;
        if let Ok(Ok(text)) = rendered {
            return ok(text);
        }
        // degrade gracefully to WAT-only
    }
    match wasm_to_wat(&bytes) {
        Ok(text) => ok(text),
        Err(message) => err(message),
    }
}

// ── strings / constants extraction ───────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default)]
pub struct StringsOptions<'a> {
    /// Minimum run length, default 4. Any number, as the tool's schema allows.
    pub min: Option<f64>,
    /// A JavaScript regex source; an empty one filters nothing.
    pub pattern: Option<&'a str>,
}

/// `strings`-style scan of a WASM body: printable-ASCII runs (data section
/// literals, symbol names, algorithm ids, keys, URLs). Deduped, in first-seen
/// order, optionally filtered by a regex source string; an invalid pattern is
/// the error.
pub fn extract_wasm_strings(buf: &[u8], opts: StringsOptions<'_>) -> Result<Vec<String>, String> {
    let min = opts.min.unwrap_or(4.0);
    let re = match opts.pattern.filter(|p| !p.is_empty()) {
        Some(pattern) => Some(js_regex(pattern, "")?),
        None => None,
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    // One past the end flushes the last run.
    let mut start = 0;
    for at in 0..=buf.len() {
        if buf.get(at).is_some_and(|b| (0x20..=0x7e).contains(b)) {
            continue;
        }
        let run = buf.get(start..at).unwrap_or_default();
        start = at + 1;
        // Printable ASCII is valid UTF-8.
        let Ok(text) = std::str::from_utf8(run) else {
            continue;
        };
        if text.len() as f64 >= min
            && re.as_ref().is_none_or(|re| re.is_match(text))
            && seen.insert(text)
        {
            out.push(text.to_owned());
        }
    }
    Ok(out)
}

// ── export ↔ JS call-site cross-reference ────────────────────────────────────

static WAT_EXPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r#"\(export{JS_SPACE}+"([^"]+)""#)).expect("the pattern is a constant")
});

/// Export names declared in a WAT dump, e.g. `(export "checksum" ...)`.
pub fn parse_wat_exports(wat: &str) -> Vec<String> {
    WAT_EXPORT
        .captures_iter(wat)
        .filter_map(|found| Some(found.get(1)?.as_str().to_owned()))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XrefHit {
    pub request_id: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportXref {
    pub export: String,
    pub referenced: bool,
    pub hits: Vec<XrefHit>,
}

/// Bridge WASM ↔ JS: for each exported function, grep the captured script
/// bodies for its name so the agent sees where (and whether) the module is
/// called — e.g. an export invoked right before a request is signed.
// shortcut: runs every grep under the caller's borrow of the store, as the
// TypeScript runs them on the main thread; snapshot the script bodies and move
// to a worker if modules with hundreds of exports make captures stall.
pub fn xref_exports(store: &TrafficStore, request_id: &str) -> Result<Vec<ExportXref>, String> {
    let bytes = get_wasm_buffer(store, request_id)?;
    let wat = wasm_to_wat(&bytes)?;
    let scripts = list_scripts(
        store,
        &ScriptFilter {
            limit: Some(200),
            ..ScriptFilter::default()
        },
    );
    let opts = GrepOptions {
        max: 5,
        before: 40,
        after: 40,
    };
    parse_wat_exports(&wat)
        .into_iter()
        .map(|name| {
            // JavaScript's `\b` only knows ASCII word characters.
            let re = Regex::new(&format!(r"(?-u:\b){}(?-u:\b)", regex::escape(&name)))
                .map_err(|e| e.to_string())?;
            let mut hits = Vec::new();
            for script in &scripts {
                let Some(body) = script.response_body.as_deref().filter(|b| !b.is_empty()) else {
                    continue;
                };
                for hit in grep_body(body, &re, &opts) {
                    hits.push(XrefHit {
                        request_id: script.request_id.clone(),
                        url: script.url.clone(),
                        snippet: hit.snippet,
                    });
                }
            }
            Ok(ExportXref {
                export: name,
                referenced: !hits.is_empty(),
                hits,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rever_core::traffic::RequestPatch;
    use rever_tools_api::utils::Content;

    use super::*;

    const WASM_MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6d]; // "\0asm"

    // A real 46-byte module exporting "checksum" (compiled from test-fixtures
    // checksum.wat); lets xref_exports run the actual WAT path.
    const MINIMAL_CHECKSUM_WASM_B64: &str =
        "AGFzbQEAAAABBwFgAn9/AX8DAgEABwwBCGNoZWNrc3VtAAAKCQEHACAAIAFqCw==";

    fn patch(id: &str, url: &str, resource_type: &str, mime: Option<&str>) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            url: Some(url.into()),
            resource_type: Some(resource_type.into()),
            mime_type: mime.map(str::to_owned),
            ..RequestPatch::default()
        }
    }

    fn binary(id: &str, url: &str, resource_type: &str, mime: &str, body: &str) -> RequestPatch {
        RequestPatch {
            response_body: Some(body.into()),
            response_body_base64: Some(true),
            ..patch(id, url, resource_type, Some(mime))
        }
    }

    /// The store the getWasmBuffer and decompileRequest suites start from.
    fn store_with_w1() -> TrafficStore {
        let mut store = TrafficStore::default();
        let body = STANDARD.encode([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
        store.upsert(
            binary(
                "w1",
                "http://127.0.0.1:8779/sign.wasm",
                "Other",
                "application/wasm",
                &body,
            ),
            1,
        );
        store
    }

    fn text_of(result: &ToolResult) -> &str {
        match result.content.first() {
            Some(Content::Text { text }) => text,
            None => "",
        }
    }

    // ── ported from wasm-analysis.test.ts ───────────────────────────────────

    // U1 — binary-body encoding decision (the refetchBody fix)

    #[test]
    fn wasm_본문은_base64로_인코딩하고_매직_바이트를_보존한다() {
        let buf = [&WASM_MAGIC[..], &[0x01, 0x00, 0x00, 0x00]].concat(); // + version
        let out = encode_refetched_body(&buf, Some("application/wasm"), None);
        assert!(out.response_body_base64);
        let round = STANDARD.decode(&out.response_body).unwrap();
        assert_eq!(round.get(..4), Some(&WASM_MAGIC[..])); // 00 61 73 6d
    }

    #[test]
    fn 텍스트_본문은_utf8로_유지한다() {
        let out = encode_refetched_body(br#"{"ok":true}"#, Some("application/json"), None);
        assert!(!out.response_body_base64);
        assert_eq!(out.response_body, r#"{"ok":true}"#);
    }

    #[test]
    fn 선언된_charset으로_euc_kr_본문을_디코딩한다() {
        // 4 bytes of euc-kr: "한글". Decoding these as utf8 yields replacement chars.
        let buf = [0xc7, 0xd1, 0xb1, 0xdb];
        assert_eq!(
            encode_refetched_body(&buf, Some("text/html"), Some("euc-kr")).response_body,
            "한글"
        );
        assert_ne!(
            encode_refetched_body(&buf, Some("text/html"), None).response_body,
            "한글"
        );
    }

    #[test]
    fn 모르는_charset_라벨이면_utf8로_대체한다() {
        let buf = "한글".as_bytes();
        assert_eq!(
            encode_refetched_body(buf, Some("text/html"), Some("x-not-a-charset")).response_body,
            "한글"
        );
    }

    #[test]
    fn utf8_bom을_제거해_본문이_json으로_파싱된다() {
        let buf = [&[0xef, 0xbb, 0xbf][..], br#"{"ok":true}"#].concat();
        let out = encode_refetched_body(&buf, Some("application/json"), Some("utf-8"));
        assert_eq!(out.response_body, r#"{"ok":true}"#);
        assert!(serde_json::from_str::<serde_json::Value>(&out.response_body).is_ok());
    }

    #[test]
    fn charset_파라미터를_읽는다() {
        assert_eq!(
            charset_from_content_type(Some("application/json; charset=EUC-KR")).as_deref(),
            Some("euc-kr")
        );
        assert_eq!(
            charset_from_content_type(Some(r#"text/html;charset="utf-8""#)).as_deref(),
            Some("utf-8")
        );
    }

    #[test]
    fn charset_선언이_없으면_none을_돌려준다() {
        assert_eq!(charset_from_content_type(Some("application/json")), None);
        assert_eq!(charset_from_content_type(None), None);
    }

    // U2 — captured-body → bytes accessor

    #[test]
    fn base64_wasm_본문을_매직_헤더가_있는_바이트로_디코딩한다() {
        let buf = get_wasm_buffer(&store_with_w1(), "w1").unwrap();
        assert_eq!(buf.get(..4), Some(&WASM_MAGIC[..]));
    }

    #[test]
    fn 모르는_request_id면_버퍼_대신_에러_문자열을_돌려준다() {
        assert!(get_wasm_buffer(&store_with_w1(), "nope").is_err());
    }

    // U3 — list_wasm filtering

    fn store_with_mixed_resources() -> TrafficStore {
        let mut store = TrafficStore::default();
        store.upsert(
            binary(
                "a",
                "http://h/sign.wasm",
                "Other",
                "application/wasm",
                "...",
            ),
            1,
        );
        store.upsert(
            RequestPatch {
                response_body: Some("x".repeat(50)),
                ..patch(
                    "b",
                    "http://h/app.js",
                    "Script",
                    Some("application/javascript"),
                )
            },
            2,
        );
        store.upsert(
            binary("c", "http://h/logo.png", "Image", "image/png", "..."),
            3,
        );
        store.upsert(
            RequestPatch {
                response_body_truncated: Some(true),
                ..binary("d", "http://h/m2.wasm", "Other", "application/wasm", "...")
            },
            4,
        );
        store
    }

    #[test]
    fn wasm_항목만_돌려주고_js와_이미지는_제외한다() {
        let store = store_with_mixed_resources();
        let mut ids: Vec<&str> = list_wasm(&store, None)
            .iter()
            .map(|r| r.request_id.as_str())
            .collect();
        ids.sort();
        assert_eq!(ids, ["a", "d"]);
    }

    #[test]
    fn 잘림_플래그를_그대로_드러낸다() {
        let store = store_with_mixed_resources();
        let rows = list_wasm(&store, None);
        let d = rows.iter().find(|r| r.request_id == "d");
        assert_eq!(d.and_then(|r| r.response_body_truncated), Some(true));
    }

    // U4 — tool error paths

    #[tokio::test]
    async fn 모르는_request_id면_던지지_않고_err를_돌려준다() {
        let traffic = Mutex::new(store_with_w1());
        let res = decompile_request(&traffic, "nope", WasmFormat::Wat).await;
        assert!(res.is_error);
        assert!(text_of(&res).contains("unknown requestId"));
    }

    // U5 — strings/constants extraction (grep_wasm)

    #[test]
    fn 최소_길이_이상의_출력_가능한_문자열을_처음_나온_순서로_중복_없이_찾는다() {
        let buf = [
            &[0x00][..],
            b"abc", // below min 4 -> dropped
            &[0x00],
            b"HMAC",
            &[0x00, 0x01],
            b"checksum",
            &[0x00],
            b"HMAC", // duplicate -> deduped
        ]
        .concat();
        let opts = StringsOptions {
            min: Some(4.0),
            pattern: None,
        };
        assert_eq!(
            extract_wasm_strings(&buf, opts),
            Ok(vec!["HMAC".to_owned(), "checksum".to_owned()])
        );
    }

    #[test]
    fn 정규식_패턴으로_거른다() {
        let buf = [&b"sign_request"[..], &[0x00], b"padding_bytes"].concat();
        let opts = StringsOptions {
            min: None,
            pattern: Some("sign"),
        };
        assert_eq!(
            extract_wasm_strings(&buf, opts),
            Ok(vec!["sign_request".to_owned()])
        );
    }

    // U6 — WAT export parsing (feeds wasm_xref)

    #[test]
    fn wat_덤프에서_export_이름을_뽑는다() {
        let wat = "(module\n  (func (;0;))\n  (export \"checksum\" (func 0))\n  (export \"sign\" (func 1)))";
        assert_eq!(parse_wat_exports(wat), ["checksum", "sign"]);
    }

    // U7 — export ↔ JS xref

    fn store_with_checksum_module() -> TrafficStore {
        let mut store = TrafficStore::default();
        // sign.wasm exporting "checksum"
        store.upsert(
            binary(
                "w1",
                "http://h/sign.wasm",
                "Other",
                "application/wasm",
                MINIMAL_CHECKSUM_WASM_B64,
            ),
            1,
        );
        store.upsert(
            RequestPatch {
                response_body: Some(
                    "const r = instance.exports.checksum(2, 3); console.log(r)".into(),
                ),
                ..patch("app", "http://h/app.js", "Script", None)
            },
            2,
        );
        store
    }

    #[test]
    fn wasm_export를_그것을_참조하는_js_호출_지점과_연결한다() {
        let res = xref_exports(&store_with_checksum_module(), "w1").unwrap();
        let cx = res.iter().find(|x| x.export == "checksum").unwrap();
        assert!(cx.referenced);
        assert!(!cx.hits.is_empty());
    }

    #[test]
    fn 모르는_request_id면_xref_대신_에러_문자열을_돌려준다() {
        assert!(xref_exports(&store_with_checksum_module(), "nope").is_err());
    }

    // ── not covered by the TypeScript tests ─────────────────────────────────

    #[test]
    fn 없는_wabt_바이너리는_던지지_않고_설치_안내를_돌려준다() {
        // The Rust stand-in for "wabt cannot be loaded": the binary is not on PATH.
        let error = run_wabt_bin("rever-no-such-wabt-tool", &WASM_MAGIC, &[]).unwrap_err();
        assert_eq!(error, WABT_INSTALL_HINT);
        let hint = error.to_lowercase();
        assert!(hint.find("wabt") < hint.find("install"));
    }

    #[tokio::test]
    async fn c_형식을_만들_수_없으면_wat로_내려간다() {
        // An empty module prints the same WAT whether or not wasm2c exists; with
        // wabt installed the C source comes back instead, and it is not WAT.
        let traffic = Mutex::new(store_with_w1());
        let wat = decompile_request(&traffic, "w1", WasmFormat::Wat).await;
        assert!(!wat.is_error);
        assert_eq!(text_of(&wat), "(module)\n");
        let c = decompile_request(&traffic, "w1", WasmFormat::C).await;
        assert!(!c.is_error);
        if wasm_to_c(&WASM_MAGIC) == Err(WABT_INSTALL_HINT.to_owned()) {
            assert_eq!(text_of(&c), "(module)\n");
        }
    }

    #[tokio::test]
    async fn 모듈이_아닌_본문은_wat_에러를_돌려준다() {
        let mut store = TrafficStore::default();
        store.upsert(
            binary(
                "bad",
                "http://h/x.wasm",
                "Other",
                "application/wasm",
                "AAAA",
            ),
            1,
        );
        let res = decompile_request(&Mutex::new(store), "bad", WasmFormat::Wat).await;
        assert!(res.is_error);
        assert!(!text_of(&res).is_empty());
    }

    #[test]
    fn 바이너리는_임시_파일_경로를_마지막_인자로_받고_출력을_돌려준다() {
        assert_eq!(run_wabt_bin("cat", b"hello", &[]), Ok("hello".to_owned()));
        // Extra arguments come before the path: `cat -n <file>` numbers the line.
        assert_eq!(
            run_wabt_bin("cat", b"hello\n", &["-n"]).map(|out| out.trim().to_owned()),
            Ok("1\thello".to_owned())
        );
    }

    #[test]
    fn 출력_없이_실패한_바이너리는_종료_코드를_알린다() {
        // `false` ignores its argument and exits 1 without output.
        assert_eq!(
            run_wabt_bin("false", b"", &[]),
            Err("false exited with code 1".to_owned())
        );
    }

    #[test]
    fn 시간이나_출력_한도를_넘긴_바이너리는_죽이고_에러를_돌려준다() {
        let slow = run_bin("sleep", &[], Path::new("5"), Duration::from_millis(100), 10);
        assert_eq!(
            slow,
            Err("sleep timed out or output exceeded 10 bytes".to_owned())
        );
        let path = std::env::temp_dir().join(format!("rever-test-{}-big.txt", std::process::id()));
        std::fs::write(&path, "0123456789abcdef").unwrap();
        let big = run_bin("cat", &[], &path, Duration::from_secs(10), 10);
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            big,
            Err("cat timed out or output exceeded 10 bytes".to_owned())
        );
    }

    #[test]
    fn 임시_파일은_실행_뒤에_지운다() {
        // `echo` prints the path it was handed.
        let path = run_wabt_bin("echo", b"x", &[]).unwrap();
        let path = Path::new(path.trim());
        assert!(path.starts_with(std::env::temp_dir()), "{path:?}");
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("wasm"));
        assert!(!path.exists());
    }

    #[test]
    fn 바이너리가_아닌_mime은_접두사로_판단한다() {
        for mime in [
            "application/octet-stream",
            "image/png",
            "video/mp4",
            "audio/ogg",
            "font/woff2",
        ] {
            assert!(encode_refetched_body(b"x", Some(mime), None).response_body_base64);
        }
        assert!(!encode_refetched_body(b"x", Some("text/plain"), None).response_body_base64);
        assert!(!encode_refetched_body(b"x", None, None).response_body_base64);
        assert!(!encode_refetched_body(b"x", Some(""), None).response_body_base64);
    }

    #[test]
    fn 결과는_camel_case_키로_직렬화한다() {
        assert_eq!(
            serde_json::to_string(&encode_refetched_body(b"x", None, None)).unwrap(),
            r#"{"responseBody":"x","responseBodyBase64":false}"#
        );
    }

    #[test]
    fn 잘못된_utf8은_대체_문자로_읽고_빈_charset은_utf8로_읽는다() {
        let out = encode_refetched_body(&[b'a', 0xff, b'b'], Some("text/plain"), Some(""));
        assert_eq!(out.response_body, "a\u{fffd}b");
        // "replacement" and "x-user-defined" are labels Node's TextDecoder
        // refuses, so utf8 is used.
        for label in ["replacement", "x-user-defined"] {
            let out = encode_refetched_body("한".as_bytes(), None, Some(label));
            assert_eq!(out.response_body, "한", "{label}");
        }
    }

    #[test]
    fn charset은_따옴표와_공백을_건너뛰고_소문자로_읽는다() {
        let parse = |text| charset_from_content_type(Some(text));
        assert_eq!(
            parse("text/html; CHARSET = 'Shift_JIS'; x=1").as_deref(),
            Some("shift_jis")
        );
        assert_eq!(parse("text/html; charset=utf-8;").as_deref(), Some("utf-8"));
        assert_eq!(parse("text/html; charset="), None);
    }

    #[test]
    fn wasm은_mime이나_url_확장자로_알아본다() {
        let request = |url: &str, mime: Option<&str>| StoredRequest {
            url: url.into(),
            mime_type: mime.map(str::to_owned),
            ..StoredRequest::default()
        };
        assert!(is_wasm(&request("http://h/x", Some("application/wasm"))));
        assert!(is_wasm(&request("http://h/A.WASM?v=1", None)));
        assert!(is_wasm(&request("http://h/a.wasm", Some("text/plain"))));
        assert!(!is_wasm(&request("http://h/a.wasm/x", None)));
        assert!(!is_wasm(&request("http://h/a.wasm#frag", None)));
        assert!(!is_wasm(&request("http://h/a.wasmx", None)));
    }

    #[test]
    fn 목록은_본문이_큰_순서이고_host로_거른다() {
        let mut store = TrafficStore::default();
        for (id, url, body) in [
            ("small", "http://a.test/s.wasm", "AAAA"),
            ("large", "http://a.test/l.wasm", "AAAAAAAAAAAA"),
            ("other", "http://b.test/o.wasm", "AAAAAAAA"),
        ] {
            store.upsert(binary(id, url, "Other", "application/wasm", body), 1);
        }
        store.upsert(patch("empty", "http://a.test/e.wasm", "Other", None), 1);
        let ids = |host| -> Vec<String> {
            list_wasm(&store, host)
                .iter()
                .map(|r| r.request_id.clone())
                .collect()
        };
        assert_eq!(ids(None), ["large", "other", "small", "empty"]);
        assert_eq!(ids(Some("a.test")), ["large", "small", "empty"]);
    }

    #[test]
    fn 본문_크기는_base64면_디코딩된_바이트_수다() {
        let sized = |body: &str, base64: bool| {
            body_byte_length(&StoredRequest {
                response_body: Some(body.into()),
                response_body_base64: Some(base64),
                ..StoredRequest::default()
            })
        };
        // Math.floor(length * 0.75) for lengths 0..=9.
        let expected = [0, 0, 1, 2, 3, 3, 4, 5, 6, 6];
        for (len, bytes) in expected.into_iter().enumerate() {
            assert_eq!(sized(&"A".repeat(len), true), bytes, "length {len}");
        }
        assert_eq!(sized("한글😀", false), 4);
        assert_eq!(body_byte_length(&StoredRequest::default()), 0);
    }

    #[test]
    fn 본문이_없거나_비었으면_안내_문구를_돌려준다() {
        let mut store = TrafficStore::default();
        store.upsert(patch("none", "http://h/a.wasm", "Other", None), 1);
        assert_eq!(
            get_wasm_buffer(&store, "none"),
            Err(
                "that request has no captured body (open the page so it loads, then try list_wasm)"
                    .to_owned()
            )
        );
        assert_eq!(
            get_wasm_buffer(&store, "nope"),
            Err("unknown requestId: nope".to_owned())
        );
    }

    #[test]
    fn base64가_아닌_본문은_글자마다_하위_바이트를_쓴다() {
        let mut store = TrafficStore::default();
        store.upsert(
            RequestPatch {
                response_body: Some("\0asm\u{ff}\u{141}".into()),
                ..patch("raw", "http://h/a.wasm", "Other", None)
            },
            1,
        );
        assert_eq!(
            get_wasm_buffer(&store, "raw"),
            Ok(vec![0x00, 0x61, 0x73, 0x6d, 0xff, 0x41])
        );
    }

    #[test]
    fn 문자열_추출은_기본_최소_길이_4와_끝의_문자열을_지킨다() {
        let buf = [&b"abc"[..], &[0x7f], b"abcd", &[0x1f], b"tail~ "].concat();
        assert_eq!(
            extract_wasm_strings(&buf, StringsOptions::default()),
            Ok(vec!["abcd".to_owned(), "tail~ ".to_owned()])
        );
        assert_eq!(
            extract_wasm_strings(&[], StringsOptions::default()),
            Ok(vec![])
        );
    }

    #[test]
    fn 문자열_추출의_최소_길이는_어떤_숫자든_받는다() {
        let buf = [&b"ab"[..], &[0x00], b"abcde"].concat();
        let with_min = |min| {
            extract_wasm_strings(
                &buf,
                StringsOptions {
                    min: Some(min),
                    pattern: None,
                },
            )
        };
        assert_eq!(with_min(2.0).map(|found| found.len()), Ok(2));
        assert_eq!(with_min(2.5).map(|found| found.len()), Ok(1));
        assert_eq!(with_min(-1.0).map(|found| found.len()), Ok(2));
        assert_eq!(with_min(f64::NAN), Ok(vec![]));
    }

    #[test]
    fn 문자열_추출의_패턴은_자바스크립트_정규식이고_잘못되면_에러다() {
        let buf = [&b"AES-256-GCM"[..], &[0x00], b"aes_key_1"].concat();
        let with_pattern = |pattern| {
            extract_wasm_strings(
                &buf,
                StringsOptions {
                    min: None,
                    pattern: Some(pattern),
                },
            )
        };
        assert_eq!(
            with_pattern(r"^AES-\d+"),
            Ok(vec!["AES-256-GCM".to_owned()])
        );
        // An empty pattern filters nothing, as `opts.pattern ? … : null` does.
        assert_eq!(with_pattern("").map(|found| found.len()), Ok(2));
        assert_eq!(with_pattern("("), Err(js_regex("(", "").unwrap_err()));
        assert!(with_pattern("(?=a)").unwrap_err().contains("lookahead"));
    }

    #[test]
    fn 빈_문자열은_최소_길이가_0이어도_한_번만_나온다() {
        let found = extract_wasm_strings(
            &[0x00, 0x00, b'a'],
            StringsOptions {
                min: Some(0.0),
                pattern: None,
            },
        );
        assert_eq!(found, Ok(vec![String::new(), "a".to_owned()]));
    }

    #[test]
    fn wat는_export와_함수_본문을_담는다() {
        let bytes = node_base64_decode(MINIMAL_CHECKSUM_WASM_B64);
        let wat = wasm_to_wat(&bytes).unwrap();
        assert_eq!(parse_wat_exports(&wat), ["checksum"]);
        assert!(wat.contains("i32.add"), "{wat}");
        assert!(wasm_to_wat(b"not wasm").is_err());
    }

    #[test]
    fn xref는_단어_경계로만_잡고_참조가_없으면_false다() {
        let mut store = store_with_checksum_module();
        store.upsert(
            RequestPatch {
                response_body: Some("var checksums = 1; my_checksum(2)".into()),
                ..patch("app", "http://h/app.js", "Script", None)
            },
            3,
        );
        let res = xref_exports(&store, "w1").unwrap();
        assert_eq!(
            res,
            [ExportXref {
                export: "checksum".into(),
                referenced: false,
                hits: vec![],
            }]
        );
    }

    #[test]
    fn xref_결과는_ts와_같은_키로_직렬화한다() {
        let res = xref_exports(&store_with_checksum_module(), "w1").unwrap();
        assert_eq!(
            serde_json::to_string(&res).unwrap(),
            r#"[{"export":"checksum","referenced":true,"hits":[{"requestId":"app","url":"http://h/app.js","snippet":"const r = instance.exports.checksum(2, 3); console.log(r)"}]}]"#
        );
    }

    #[test]
    fn 요약은_타입과_export와_함수_이름을_보여준다() {
        let bytes = node_base64_decode(MINIMAL_CHECKSUM_WASM_B64);
        assert_eq!(
            wasm_objdump(&bytes).unwrap(),
            "\nmodule.wasm:\tfile format wasm 0x1\n\nSection Details:\n\n\
             Type[1]:\n - type[0] (i32, i32) -> i32\n\
             Function[1]:\n - func[0] sig=0 <checksum>\n\
             Export[1]:\n - func[0] <checksum> -> \"checksum\"\n\
             Code[1]:\n - func[0] size=7 <checksum>\n"
        );
        assert!(wasm_objdump(b"not wasm").is_err());
    }

    #[test]
    fn 요약은_import와_메모리와_전역과_데이터와_이름_섹션을_읽는다() {
        // (module
        //   (import "env" "log" (func $log (param i32)))
        //   (memory 1 2) (global (mut i32) (i32.const 66560))
        //   (func $run) (export "memory" (memory 0)) (export "run" (func $run))
        //   (start $run) (data (i32.const 1024) "hello"))
        // with a name section naming func 1 "run_impl".
        let module: Vec<u8> = [
            &WASM_MAGIC[..],
            &[0x01, 0x00, 0x00, 0x00],
            // type: (i32) -> nil, () -> nil
            &[0x01, 0x08, 0x02, 0x60, 0x01, 0x7f, 0x00, 0x60, 0x00, 0x00],
            // import env.log func sig 0
            &[
                0x02, 0x0b, 0x01, 0x03, b'e', b'n', b'v', 0x03, b'l', b'o', b'g', 0x00, 0x00,
            ],
            // function: one, sig 1
            &[0x03, 0x02, 0x01, 0x01],
            // memory: min 1 max 2
            &[0x05, 0x04, 0x01, 0x01, 0x01, 0x02],
            // global: mut i32 = 66560
            &[0x06, 0x08, 0x01, 0x7f, 0x01, 0x41, 0x80, 0x88, 0x04, 0x0b],
            // export "memory" memory 0, "run" func 1
            &[
                0x07, 0x10, 0x02, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
            ],
            &[0x03, b'r', b'u', b'n', 0x00, 0x01],
            // start func 1
            &[0x08, 0x01, 0x01],
            // code: one empty body
            &[0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b],
            // data: active memory 0 at 1024, "hello"
            &[0x0b, 0x0c, 0x01, 0x00, 0x41, 0x80, 0x08, 0x0b, 0x05],
            b"hello",
            // custom "name": function names { 1: "run_impl" }
            &[
                0x00, 0x12, 0x04, b'n', b'a', b'm', b'e', 0x01, 0x0b, 0x01, 0x01, 0x08,
            ],
            b"run_impl",
        ]
        .concat();
        assert_eq!(
            wasm_objdump(&module).unwrap(),
            "\nmodule.wasm:\tfile format wasm 0x1\n\nSection Details:\n\n\
             Type[2]:\n - type[0] (i32) -> nil\n - type[1] () -> nil\n\
             Import[1]:\n - func[0] sig=0 <env.log> <- env.log\n\
             Function[1]:\n - func[1] sig=1 <run_impl>\n\
             Memory[1]:\n - memory[0] pages: initial=1 max=2\n\
             Global[1]:\n - global[0] i32 mutable=1 - init i32=66560\n\
             Export[2]:\n - memory[0] -> \"memory\"\n - func[1] <run_impl> -> \"run\"\n\
             Start:\n - start function: 1\n\
             Code[1]:\n - func[1] size=2 <run_impl>\n\
             Data[1]:\n - segment[0] memory=0 size=5 - init i32=1024\n\
             Custom:\n - name: \"name\"\n - func[1] <run_impl>\n"
        );
    }
}
