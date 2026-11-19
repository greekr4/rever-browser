//! Port of `src/main/mcp/tools/sourcemap.ts` — resolve_source, list_sources and
//! get_original_source: map a position in a captured bundle back to the
//! original source through the bundle's source map.
//!
//! The TypeScript reads maps with `@jridgewell/trace-mapping`; [`TraceMap`]
//! here does what the tools use of it (decoding the mappings, the lookup on a
//! generated line, and the normalised source names) the way that library does.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use regex::Regex;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::{Value, json};

use crate::args::{Args, MAX_SAFE_INTEGER};
use crate::js::{JS_NOT_SPACE, JS_SPACE, node_base64_decode};
use crate::{ok_pretty, traffic};

/// The parsed maps, by script requestId (the TypeScript's module-level cache).
#[derive(Default)]
pub struct SourceMapCache {
    maps: Mutex<HashMap<String, Arc<TraceMap>>>,
}

fn offset_to_line_col(body: &str, offset: usize) -> (usize, usize) {
    let (mut line, mut col) = (0, 0);
    // UTF-16 units, as the TypeScript's string index.
    for unit in body.encode_utf16().take(offset) {
        if unit == u16::from(b'\n') {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

// ── the source map itself ────────────────────────────────────────────────────

/// One mapping on a generated line. `source` is absent for a segment that
/// maps to nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Segment {
    column: i64,
    /// (sources index, 0-based line, column)
    source: Option<(i64, i64, i64)>,
    name: Option<i64>,
}

#[derive(Debug)]
pub struct TraceMap {
    /// As written in the map (`null` entries included): what list_sources shows.
    pub sources: Vec<Value>,
    /// Normalised with `sourceRoot`: what a resolved position reports.
    resolved_sources: Vec<String>,
    sources_content: Option<Vec<Value>>,
    names: Vec<Value>,
    /// Segments per generated line, sorted by column.
    decoded: Vec<Vec<Segment>>,
}

/// Value of one base64 VLQ digit; anything else reads as 0, as in the library.
fn vlq_digit(byte: Option<&u8>) -> i32 {
    match byte {
        Some(b @ b'A'..=b'Z') => i32::from(b - b'A'),
        Some(b @ b'a'..=b'z') => i32::from(b - b'a') + 26,
        Some(b @ b'0'..=b'9') => i32::from(b - b'0') + 52,
        Some(b'+') => 62,
        Some(b'/') => 63,
        _ => 0,
    }
}

/// Reads one VLQ at `pos` and adds it to `relative`. The 32-bit wrapping is the
/// library's (JavaScript bit operations), so a malformed map decodes to the
/// same nonsense rather than panicking.
fn decode_integer(units: &[u8], pos: &mut usize, relative: i64) -> i64 {
    let mut value: i32 = 0;
    let mut shift: u32 = 0;
    loop {
        let digit = vlq_digit(units.get(*pos));
        *pos += 1;
        value |= (digit & 31).wrapping_shl(shift);
        shift = shift.wrapping_add(5);
        if digit & 32 == 0 {
            break;
        }
    }
    let negate = value & 1 != 0;
    value = ((value as u32) >> 1) as i32;
    if negate {
        value = i32::MIN | value.wrapping_neg();
    }
    relative.saturating_add(i64::from(value))
}

fn decode_mappings(mappings: &str) -> Vec<Vec<Segment>> {
    // The library reads UTF-16 units from one cursor over the whole string: a
    // unit outside ASCII is digit 0, and a segment cut short keeps reading
    // past the `;` into the next line. Malformed maps decode the same way.
    let units: Vec<u8> = mappings
        .encode_utf16()
        .map(|unit| u8::try_from(unit).unwrap_or(u8::MAX))
        .collect();
    let (mut sources_index, mut source_line, mut source_column, mut names_index) = (0, 0, 0, 0);
    let mut decoded = Vec::new();
    let mut pos = 0;
    loop {
        let rest = units.get(pos..).unwrap_or_default();
        let semi = pos + rest.iter().position(|b| *b == b';').unwrap_or(rest.len());
        let more = |pos: usize| pos < semi && units.get(pos) != Some(&b',');
        let mut segments = Vec::new();
        let mut column = 0;
        while pos < semi {
            column = decode_integer(&units, &mut pos, column);
            let mut segment = Segment {
                column,
                source: None,
                name: None,
            };
            if more(pos) {
                sources_index = decode_integer(&units, &mut pos, sources_index);
                source_line = decode_integer(&units, &mut pos, source_line);
                source_column = decode_integer(&units, &mut pos, source_column);
                segment.source = Some((sources_index, source_line, source_column));
                if more(pos) {
                    names_index = decode_integer(&units, &mut pos, names_index);
                    segment.name = Some(names_index);
                }
            }
            segments.push(segment);
            pos += 1;
        }
        // Stable, like Array.prototype.sort: segments at one column keep their order.
        segments.sort_by_key(|segment| segment.column);
        decoded.push(segments);
        pos = semi + 1;
        if pos > units.len() {
            return decoded;
        }
    }
}

/// Mappings already decoded in the JSON: arrays of 1, 4 or 5 numbers per segment.
fn segments_from_json(lines: &[Value]) -> Vec<Vec<Segment>> {
    let number = |value: Option<&Value>| value.and_then(Value::as_i64).unwrap_or(0);
    lines
        .iter()
        .map(|line| {
            let mut segments: Vec<Segment> = line
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_array)
                .map(|fields| Segment {
                    column: number(fields.first()),
                    source: (fields.len() > 1).then(|| {
                        (
                            number(fields.get(1)),
                            number(fields.get(2)),
                            number(fields.get(3)),
                        )
                    }),
                    name: (fields.len() == 5).then(|| number(fields.get(4))),
                })
                .collect();
            segments.sort_by_key(|segment| segment.column);
            segments
        })
        .collect()
}

/// `scheme://user@host:port` at the start of an absolute URL.
static ABSOLUTE_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_+.-]+://(?:[^@/#?]*@)?[^:/#?]*(?::[0-9]+)?")
        .expect("the pattern is a constant")
});

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum UrlKind {
    Hash,
    Query,
    RelativePath,
    AbsolutePath,
    SchemeRelative,
    Absolute,
}

/// `path`, `?query` and `#hash` of what follows the authority of a URL.
fn split_path(input: &str) -> (&str, &str, &str) {
    let cut = |text, at: Option<usize>| match at.and_then(|at| str::split_at_checked(text, at)) {
        Some(halves) => halves,
        None => (text, ""),
    };
    let (rest, hash) = cut(input, input.find('#'));
    let (path, query) = cut(rest, rest.find('?'));
    (path, query, hash)
}

/// Drops `.` and empty pieces and folds `..` into its parent. A relative path
/// keeps the `..` it cannot fold; an absolute one drops them.
fn normalize_path(path: &str, relative: bool) -> String {
    let mut pieces: Vec<&str> = Vec::new();
    let mut positive = 0;
    let mut trailing_slash = false;
    for piece in path.split('/').skip(1) {
        if piece.is_empty() {
            trailing_slash = true;
            continue;
        }
        trailing_slash = false;
        match piece {
            "." => {}
            ".." if positive > 0 => {
                trailing_slash = true;
                positive -= 1;
                pieces.pop();
            }
            ".." if relative => pieces.push(piece),
            ".." => {}
            _ => {
                pieces.push(piece);
                positive += 1;
            }
        }
    }
    let mut out: String = pieces.iter().flat_map(|piece| ["/", piece]).collect();
    if out.is_empty() || (trailing_slash && !out.ends_with("/..")) {
        out.push('/');
    }
    out
}

/// `@jridgewell/resolve-uri` with no base, which is how trace-mapping resolves
/// the sources of a map it was given without its URL: the name is normalised
/// (`./a/../b.ts` → `./b.ts`, `webpack:///./src/x.ts` → `webpack:///src/x.ts`).
fn resolve_uri(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    // The authority and where the URL ends: what follows `scheme://user@host:port`
    // counts only when it is a path, query or hash; the library's pattern reads
    // anything else there (a port that is not a number) as nothing.
    let after = |authority: regex::Match<'_>, text: &'_ str| -> Option<(String, usize)> {
        let rest = text.get(authority.end()..)?;
        let end = if rest.starts_with(['/', '?', '#']) {
            text.len()
        } else {
            authority.end()
        };
        Some((authority.as_str().to_owned(), end))
    };
    let with_scheme;
    // `origin` is everything before the path.
    let (kind, origin, path, query, hash) = if input.starts_with("//") {
        with_scheme = format!("http:{input}");
        let (origin, end) = ABSOLUTE_URL
            .find(&with_scheme)
            .and_then(|authority| after(authority, &with_scheme))
            .unwrap_or_default();
        let rest = with_scheme.get(origin.len()..end).unwrap_or("");
        let (path, query, hash) = split_path(rest);
        let origin = origin.get("http:".len()..).unwrap_or("").to_owned();
        (
            UrlKind::SchemeRelative,
            origin,
            path.to_owned(),
            query,
            hash,
        )
    } else if input.starts_with('/') {
        let (path, query, hash) = split_path(input);
        (
            UrlKind::AbsolutePath,
            String::new(),
            path.to_owned(),
            query,
            hash,
        )
    } else if let Some(rest) = input.strip_prefix("file:") {
        // File URLs need no `//`, may have a host, and a Windows drive letter
        // right after `//` is the start of the path, not a host.
        let (host, rest) = match rest.strip_prefix("//") {
            Some(after) => {
                let bytes = after.as_bytes();
                let drive = bytes.first().is_some_and(u8::is_ascii_alphabetic)
                    && bytes.get(1) == Some(&b':');
                let end = after.find(['/', '#', '?']).unwrap_or(after.len());
                match after.split_at_checked(end) {
                    Some(halves) if !drive => halves,
                    _ => ("", after),
                }
            }
            None => ("", rest),
        };
        let (path, query, hash) = split_path(rest);
        let path = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };
        (
            UrlKind::Absolute,
            format!("file://{host}"),
            path,
            query,
            hash,
        )
    } else if let Some((origin, end)) = ABSOLUTE_URL
        .find(input)
        .and_then(|authority| after(authority, input))
    {
        let (path, query, hash) = split_path(input.get(origin.len()..end).unwrap_or(""));
        (UrlKind::Absolute, origin, path.to_owned(), query, hash)
    } else {
        let (path, query, hash) = split_path(input);
        let kind = match input.chars().next() {
            Some('?') => UrlKind::Query,
            Some('#') => UrlKind::Hash,
            _ => UrlKind::RelativePath,
        };
        (kind, String::new(), format!("/{path}"), query, hash)
    };
    let path = normalize_path(&path, kind <= UrlKind::RelativePath);
    let query_hash = format!("{query}{hash}");
    match kind {
        UrlKind::Hash | UrlKind::Query => query_hash,
        UrlKind::RelativePath => {
            // The first char is always a "/", and we need it to be relative.
            let path = path.get(1..).unwrap_or("");
            let is_relative = |text: &str| text.starts_with(['.', '?', '#']);
            if path.is_empty() {
                if query_hash.is_empty() {
                    ".".to_owned()
                } else {
                    query_hash
                }
            } else if is_relative(input) && !is_relative(path) {
                // The input started with a ".", so the result must too.
                format!("./{path}{query_hash}")
            } else {
                format!("{path}{query_hash}")
            }
        }
        UrlKind::AbsolutePath => format!("{path}{query_hash}"),
        UrlKind::SchemeRelative | UrlKind::Absolute => format!("{origin}{path}{query_hash}"),
    }
}

impl TraceMap {
    /// `new TraceMap(parsed)`: the parsed JSON of a source map, or a string
    /// holding that JSON.
    pub fn new(map: &Value) -> Result<Self, String> {
        let reparsed;
        let map = match map {
            Value::String(text) => {
                reparsed = serde_json::from_str::<Value>(text).map_err(|e| e.to_string())?;
                &reparsed
            }
            other => other,
        };
        let invalid = || format!("invalid source map: {map}");
        let Some(sources) = map.get("sources").and_then(Value::as_array) else {
            return Err(invalid());
        };
        // The sourceRoot is always treated as a directory, if it's not empty.
        let prefix = match map.get("sourceRoot").and_then(Value::as_str) {
            Some(root) if !root.is_empty() => format!("{root}/"),
            _ => String::new(),
        };
        let resolved_sources = sources
            .iter()
            .map(|source| resolve_uri(&format!("{prefix}{}", source.as_str().unwrap_or(""))))
            .collect();
        let decoded = match map.get("mappings") {
            Some(Value::String(mappings)) => decode_mappings(mappings),
            Some(Value::Array(lines)) => segments_from_json(lines),
            _ if map.get("sections").is_some() => {
                return Err(
                    "TraceMap passed sectioned source map, please use FlattenMap export instead"
                        .into(),
                );
            }
            _ => return Err(invalid()),
        };
        Ok(Self {
            sources: sources.clone(),
            resolved_sources,
            sources_content: map.get("sourcesContent").and_then(Value::as_array).cloned(),
            names: map
                .get("names")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            decoded,
        })
    }

    /// `sourceContentFor`: the embedded content of a source, named as written
    /// in the map or as a resolved position reports it.
    pub fn source_content_for(&self, source: &str) -> Option<&str> {
        let contents = self.sources_content.as_ref()?;
        let index = self
            .sources
            .iter()
            .position(|s| s.as_str() == Some(source))
            .or_else(|| self.resolved_sources.iter().position(|s| s == source))?;
        contents.get(index)?.as_str()
    }
}

/// A resolved position: `line` is 1-based, `column` 0-based, all four null
/// where the generated position maps to nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OriginalPosition {
    pub source: Option<String>,
    pub line: Option<i64>,
    pub column: Option<i64>,
    pub name: Option<String>,
    // A segment whose index is outside the map's `sources` / `names` reads
    // JavaScript's undefined, which JSON.stringify leaves out; a position that
    // maps to nothing has nulls instead.
    source_undefined: bool,
    name_undefined: bool,
}

impl Serialize for OriginalPosition {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        if !self.source_undefined {
            map.serialize_entry("source", &self.source)?;
        }
        map.serialize_entry("line", &self.line)?;
        map.serialize_entry("column", &self.column)?;
        if !self.name_undefined {
            map.serialize_entry("name", &self.name)?;
        }
        map.end()
    }
}

// trace-mapping takes a 1-based line and a 0-based column; this tool's input
// (and CDP locations, and offset_to_line_col) are 0-based on both. Passing the
// line through unconverted resolved one generated line too early.
pub fn original_at(tm: &TraceMap, line0: usize, column0: usize) -> OriginalPosition {
    let unmapped = OriginalPosition::default();
    // It's common for a position to point at a line that has no mapping (like
    // a "//# sourceMappingURL=" at the end of the file).
    let Some(segments) = tm.decoded.get(line0) else {
        return unmapped;
    };
    let column = i64::try_from(column0).unwrap_or(i64::MAX);
    // The first segment at exactly this column, else the last one before it.
    let at = segments.partition_point(|segment| segment.column < column);
    let exact = segments.get(at).filter(|segment| segment.column == column);
    let Some(segment) = exact.or_else(|| segments.get(at.checked_sub(1)?)) else {
        return unmapped;
    };
    let Some((sources_index, line, column)) = segment.source else {
        return unmapped;
    };
    let source = usize::try_from(sources_index)
        .ok()
        .and_then(|index| tm.resolved_sources.get(index));
    // `Some(None)`: the segment names an entry the map does not have.
    let name = segment
        .name
        .map(|index| tm.names.get(usize::try_from(index).ok()?));
    OriginalPosition {
        source: source.cloned(),
        line: Some(line.saturating_add(1)),
        column: Some(column),
        name: name.flatten().and_then(Value::as_str).map(str::to_owned),
        source_undefined: source.is_none(),
        name_undefined: name == Some(None),
    }
}

// ── loading ──────────────────────────────────────────────────────────────────

static MAPPING_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"//[#@]{JS_SPACE}*sourceMappingURL=({JS_NOT_SPACE}+)"
    ))
    .expect("the pattern is a constant")
});

/// `decodeURIComponent`; the error is the message JavaScript throws with.
fn decode_uri_component(text: &str) -> Result<String, String> {
    let malformed = || "URI malformed".to_owned();
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(byte) = bytes.get(at) {
        if *byte != b'%' {
            out.push(*byte);
            at += 1;
            continue;
        }
        let hex = text.get(at + 1..at + 3).ok_or_else(malformed)?;
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(malformed());
        }
        out.push(u8::from_str_radix(hex, 16).map_err(|_| malformed())?);
        at += 3;
    }
    String::from_utf8(out).map_err(|_| malformed())
}

/// The JSON text of a `data:` source map URL.
fn data_url_json(mapping_url: &str) -> Result<String, String> {
    // data:application/json;base64,<...>
    let base64 = mapping_url
        .find("base64,")
        .and_then(|at| mapping_url.get(at + "base64,".len()..))
        .filter(|rest| !rest.is_empty());
    if let Some(base64) = base64 {
        return Ok(String::from_utf8_lossy(&node_base64_decode(base64)).into_owned());
    }
    // data:application/json,...
    match mapping_url.split_once(',') {
        Some((_, rest)) => decode_uri_component(rest),
        None => Ok(String::new()),
    }
}

async fn fetch_map(fetch_url: &str) -> Result<String, String> {
    let failed = |e: reqwest::Error| format!("fetch source map failed: {e}");
    // Node's fetch does not read proxy settings from the environment.
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(failed)?;
    let resp = client.get(fetch_url).send().await.map_err(failed)?;
    if !resp.status().is_success() {
        return Err(format!(
            "failed to fetch source map {fetch_url}: {}",
            resp.status().as_u16()
        ));
    }
    let text = resp.text().await.map_err(failed)?;
    // fetch's text() drops a leading BOM.
    Ok(match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    })
}

async fn load_source_map(
    ctx: &ToolContext,
    script_request_id: &str,
) -> Result<Arc<TraceMap>, String> {
    let cache = ctx.state::<SourceMapCache>();
    let cached = |cache: &SourceMapCache| {
        let maps = cache.maps.lock().unwrap_or_else(|p| p.into_inner());
        maps.get(script_request_id).cloned()
    };
    if let Some(tm) = cached(&cache) {
        return Ok(tm);
    }

    // The store is unlocked again before anything is fetched.
    let (script_url, mapping_url) = {
        let store = traffic(ctx);
        let Some(entry) = store.get(script_request_id) else {
            return Err(format!("unknown requestId: {script_request_id}"));
        };
        let Some(body) = entry.response_body.as_deref().filter(|b| !b.is_empty()) else {
            return Err(format!("no response body for {script_request_id}"));
        };
        if entry.response_body_base64 == Some(true) {
            return Err(format!("body is base64 (binary) for {script_request_id}"));
        }
        // Find sourceMappingURL comment
        let Some(mapping_url) = MAPPING_URL.captures(body).and_then(|found| found.get(1)) else {
            return Err(format!("no sourceMappingURL found in {script_request_id}"));
        };
        (entry.url.clone(), mapping_url.as_str().to_owned())
    };

    let map_json = if mapping_url.starts_with("data:") {
        data_url_json(&mapping_url)?
    } else {
        // External URL — resolve relative to script URL
        let fetch_url = if mapping_url.starts_with("http") {
            mapping_url
        } else {
            reqwest::Url::parse(&script_url)
                .and_then(|base| base.join(&mapping_url))
                .map_err(|_| format!("cannot resolve source map URL: {mapping_url}"))?
                .to_string()
        };
        fetch_map(&fetch_url).await?
    };

    let parsed = serde_json::from_str::<Value>(&map_json)
        .map_err(|e| e.to_string())
        .and_then(|parsed| TraceMap::new(&parsed))
        .map_err(|e| format!("failed to parse source map: {e}"))?;
    let tm = Arc::new(parsed);
    let mut maps = cache.maps.lock().unwrap_or_else(|p| p.into_inner());
    maps.insert(script_request_id.to_owned(), Arc::clone(&tm));
    Ok(tm)
}

// ── tools ────────────────────────────────────────────────────────────────────

pub struct ResolveSource;

impl Tool for ResolveSource {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "resolve_source",
            "Resolve a byte offset or 0-indexed line/column in a captured script to its original source file and position using the embedded source map. The returned line is 1-based, column 0-based.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": { "type": "string", "description": "requestId of the captured script" },
                    "byteOffset": { "type": "integer", "minimum": 0, "maximum": MAX_SAFE_INTEGER, "description": "Byte offset in the bundle (takes precedence over line/column)" },
                    "line": { "type": "integer", "minimum": 0, "maximum": MAX_SAFE_INTEGER, "description": "0-indexed line number" },
                    "column": { "type": "integer", "minimum": 0, "maximum": MAX_SAFE_INTEGER, "description": "0-indexed column number" }
                },
                "required": ["requestId"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { resolve_source(ctx, &args).await.unwrap_or_else(err) })
    }
}

async fn resolve_source(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("resolve_source", args)?;
    let request_id = args.str("requestId")?;
    let byte_offset = args.opt_int("byteOffset", 0, None)?;
    let mut line = args.opt_int("line", 0, None)?.unwrap_or(0);
    let mut column = args.opt_int("column", 0, None)?.unwrap_or(0);
    let tm = load_source_map(ctx, request_id).await?;

    if let Some(offset) = byte_offset {
        let store = traffic(ctx);
        let body = store
            .get(request_id)
            .and_then(|entry| entry.response_body.as_deref())
            .filter(|body| !body.is_empty());
        if let Some(body) = body {
            (line, column) = offset_to_line_col(body, offset);
        }
    }

    let orig = original_at(&tm, line, column);

    // Get snippet from sourcesContent if available
    let content = orig
        .source
        .as_deref()
        .and_then(|source| tm.source_content_for(source))
        .filter(|content| !content.is_empty());
    let snippet = content.map(|content| {
        let lines: Vec<&str> = content.split('\n').collect();
        let sl = usize::try_from(orig.line.unwrap_or(0)).unwrap_or(0);
        let start = sl.saturating_sub(5);
        let end = lines.len().min(sl.saturating_add(5));
        lines.get(start..end).unwrap_or_default().join("\n")
    });

    let mut out = serde_json::to_value(&orig).map_err(|e| e.to_string())?;
    if let (Some(fields), Some(snippet)) = (out.as_object_mut(), snippet) {
        fields.insert("snippet".into(), Value::from(snippet));
    }
    Ok(ok_pretty(&out))
}

pub struct ListSources;

impl Tool for ListSources {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "list_sources",
            "List all original source files referenced in a script's source map.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": { "type": "string", "description": "requestId of the captured script" }
                },
                "required": ["requestId"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { list_sources(ctx, &args).await.unwrap_or_else(err) })
    }
}

async fn list_sources(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("list_sources", args)?;
    let tm = load_source_map(ctx, args.str("requestId")?).await?;
    Ok(ok_pretty(&json!({ "sources": tm.sources })))
}

pub struct GetOriginalSource;

impl Tool for GetOriginalSource {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "get_original_source",
            "Return the full original source content for a file listed by list_sources.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": { "type": "string", "description": "requestId of the captured script" },
                    "source": { "type": "string", "description": "Source file path as returned by list_sources" }
                },
                "required": ["requestId", "source"]
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { get_original_source(ctx, &args).await.unwrap_or_else(err) })
    }
}

async fn get_original_source(ctx: &ToolContext, args: &Value) -> Result<ToolResult, String> {
    let args = Args::new("get_original_source", args)?;
    let request_id = args.str("requestId")?;
    let source = args.str("source")?;
    let tm = load_source_map(ctx, request_id).await?;
    match tm.source_content_for(source) {
        Some(content) => Ok(ok(content)),
        None => Err(format!("no sourcesContent for {source}")),
    }
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ResolveSource);
    registry.register(ListSources);
    registry.register(GetOriginalSource);
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use rever_core::traffic::RequestPatch;
    use rever_tools_api::utils::Content;

    use super::*;

    // ── ported from sourcemap.test.ts ───────────────────────────────────────

    // 생성 코드 3번째 줄(0-based 2) 4번째 칸(0-based 4) → 원본 app.ts 11번째 줄(0-based 10) 2번째 칸
    fn fixture() -> TraceMap {
        TraceMap::new(&json!({
            "version": 3,
            "sources": ["app.ts"],
            "names": [],
            "mappings": ";;IAUE"
        }))
        .unwrap()
    }

    #[test]
    fn 영_기반_line과_column을_받아_원본_위치를_찾는다() {
        let position = original_at(&fixture(), 2, 4);
        assert_eq!(position.source.as_deref(), Some("app.ts"));
        assert_eq!(position.line, Some(11));
        assert_eq!(position.column, Some(2));
    }

    #[test]
    fn 매핑이_없는_줄은_source가_null이다() {
        assert_eq!(original_at(&fixture(), 1, 4).source, None);
    }

    // ── not covered by the TypeScript tests ─────────────────────────────────

    const MAP: &str = r#"{"version":3,"sources":["webpack:///./src/app.ts","lib/util.ts"],"names":["sign","key"],"sourcesContent":["l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nl11\nl12\nl13\nl14\nl15\nl16\nl17",null],"mappings":"AAAA;IAUEA,KAAKC;;ACVP"}"#;

    fn script(id: &str, url: &str, body: &str) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            url: Some(url.into()),
            resource_type: Some("Script".into()),
            response_body: Some(body.into()),
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

    fn inline(id: &str, generated: &str, map: &str) -> RequestPatch {
        let url = format!(
            "data:application/json;charset=utf-8;base64,{}",
            STANDARD.encode(map)
        );
        script(
            id,
            "http://h/static/app.js",
            &format!("{generated}\n//# sourceMappingURL={url}\n"),
        )
    }

    async fn call(tool: impl Tool, ctx: &ToolContext, args: Value) -> (bool, String) {
        let result = tool.call(ctx, args).await;
        let text = match result.content.first() {
            Some(Content::Text { text }) => text.clone(),
            None => String::new(),
        };
        (result.is_error, text)
    }

    /// Serves one HTTP response on a local port and returns its base URL.
    fn serve_once(status: &str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_owned();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = [0; 2048];
            let _ = stream.read(&mut request);
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        format!("http://{address}")
    }

    #[test]
    fn 열이_정확히_맞으면_그_세그먼트를_아니면_바로_앞_세그먼트를_쓴다() {
        let tm = TraceMap::new(&serde_json::from_str(MAP).unwrap()).unwrap();
        // Line 1 holds segments at columns 4 and 9.
        let at = |column| original_at(&tm, 1, column);
        assert_eq!(at(3), OriginalPosition::default());
        assert_eq!(
            at(4),
            OriginalPosition {
                source: Some("webpack:///src/app.ts".into()),
                line: Some(11),
                column: Some(2),
                name: Some("sign".into()),
                ..OriginalPosition::default()
            }
        );
        assert_eq!(at(8), at(4));
        assert_eq!(
            at(500),
            OriginalPosition {
                source: Some("webpack:///src/app.ts".into()),
                line: Some(11),
                column: Some(7),
                name: Some("key".into()),
                ..OriginalPosition::default()
            }
        );
        // Line 2 is empty, line 3 maps to the second source without a name.
        assert_eq!(original_at(&tm, 2, 0), OriginalPosition::default());
        assert_eq!(
            original_at(&tm, 3, 0),
            OriginalPosition {
                source: Some("lib/util.ts".into()),
                line: Some(1),
                column: Some(0),
                name: None,
                ..OriginalPosition::default()
            }
        );
        assert_eq!(original_at(&tm, 99, 0), OriginalPosition::default());
    }

    #[test]
    fn 원본이_없는_세그먼트와_뒤섞인_세그먼트를_읽는다() {
        // Columns 4 (mapped), 5 (unmapped) and, one back, 4 again: out of order.
        let tm = TraceMap::new(&json!({ "sources": ["a.ts"], "mappings": "IAAA,C,DAAC" })).unwrap();
        assert_eq!(original_at(&tm, 0, 5), OriginalPosition::default());
        // The third segment decodes to column 4 again and sorts after the first.
        assert_eq!(original_at(&tm, 0, 4).column, Some(0));
        assert_eq!(original_at(&tm, 0, 0), OriginalPosition::default());
    }

    #[test]
    fn 깨진_매핑도_패닉_없이_읽는다() {
        for mappings in [
            "",
            ";;;",
            "!!!!",
            "g",
            "gggggggggggggggggggg",
            "한글,;A",
            "AAAA,,,,",
            ",",
        ] {
            let tm = TraceMap::new(&json!({ "sources": [], "mappings": mappings })).unwrap();
            for line in 0..4 {
                let _ = original_at(&tm, line, 0);
                let _ = original_at(&tm, line, usize::MAX);
            }
        }
        // A source index outside `sources` is a position without a source.
        let tm = TraceMap::new(&json!({ "sources": [], "mappings": "AAAA" })).unwrap();
        assert_eq!(
            original_at(&tm, 0, 0),
            OriginalPosition {
                source: None,
                line: Some(1),
                column: Some(0),
                name: None,
                source_undefined: true,
                name_undefined: false,
            }
        );
    }

    #[test]
    fn 깨진_매핑을_라이브러리와_같은_값으로_읽는다() {
        // Expected values are what @jridgewell/trace-mapping returns.
        let at = |mappings: &str, line, column| {
            let map = json!({ "sources": ["a.ts"], "names": ["n"], "mappings": mappings });
            serde_json::to_string(&original_at(&TraceMap::new(&map).unwrap(), line, column))
                .unwrap()
        };
        // A segment cut short reads on past the `;`: the "C" of the next line
        // is its source column, and stays the running column after that.
        let expected = r#"{"source":"a.ts","line":1,"column":1,"name":null}"#;
        assert_eq!(at("AA;CAAA", 0, 0), expected);
        assert_eq!(at("AA;CAAA", 1, 1), expected);
        // A character outside ASCII is one digit 0, not one per UTF-8 byte.
        assert_eq!(
            at("한AAA", 0, 0),
            r#"{"source":"a.ts","line":1,"column":0,"name":null}"#
        );
    }

    #[test]
    fn 맵에_없는_소스나_이름을_가리키면_그_키를_빼고_직렬화한다() {
        // Expected texts are what JSON.stringify prints for trace-mapping's
        // result: undefined (index out of range) is left out, null is kept.
        let tm = TraceMap::new(&json!({
            "sources": ["a.ts"],
            "names": ["n"],
            "mappings": "AAAA,CCAA,CDAAE;AAAAF;A"
        }))
        .unwrap();
        let text = |line, column| serde_json::to_string(&original_at(&tm, line, column)).unwrap();
        assert_eq!(
            text(0, 0),
            r#"{"source":"a.ts","line":1,"column":0,"name":null}"#
        );
        assert_eq!(text(0, 1), r#"{"line":1,"column":0,"name":null}"#);
        assert_eq!(text(0, 2), r#"{"source":"a.ts","line":1,"column":0}"#);
        assert_eq!(
            text(1, 0),
            r#"{"source":"a.ts","line":1,"column":0,"name":"n"}"#
        );
        assert_eq!(
            text(2, 0),
            r#"{"source":null,"line":null,"column":null,"name":null}"#
        );
    }

    #[test]
    fn 디코딩된_매핑_배열과_문자열로_감싼_맵도_받는다() {
        let decoded = json!({ "sources": ["a.ts"], "names": ["n"], "mappings": [[[9, 0, 4, 1, 0], [2]], []] });
        let tm = TraceMap::new(&decoded).unwrap();
        assert_eq!(original_at(&tm, 0, 3), OriginalPosition::default());
        assert_eq!(
            original_at(&tm, 0, 9),
            OriginalPosition {
                source: Some("a.ts".into()),
                line: Some(5),
                column: Some(1),
                name: Some("n".into()),
                ..OriginalPosition::default()
            }
        );
        let wrapped = Value::from(r#"{"sources":["a.ts"],"mappings":"AAAA"}"#);
        assert_eq!(
            original_at(&TraceMap::new(&wrapped).unwrap(), 0, 0)
                .source
                .as_deref(),
            Some("a.ts")
        );
    }

    #[test]
    fn 소스맵이_아닌_json은_에러다() {
        assert_eq!(
            TraceMap::new(&json!({ "sources": [], "sections": [] })).unwrap_err(),
            "TraceMap passed sectioned source map, please use FlattenMap export instead"
        );
        assert_eq!(
            TraceMap::new(&json!({ "sources": [] })).unwrap_err(),
            r#"invalid source map: {"sources":[]}"#
        );
        for value in [json!(null), json!(3), json!([]), json!({ "mappings": "" })] {
            assert!(TraceMap::new(&value).is_err(), "{value}");
        }
        assert!(TraceMap::new(&Value::from("{oops")).is_err());
    }

    #[test]
    fn 소스_이름은_source_root를_붙여_정규화한다() {
        // Expected values are what @jridgewell/resolve-uri returns without a base.
        let cases = [
            ("app.ts", "app.ts"),
            ("./app.ts", "./app.ts"),
            ("./a/../b.ts", "./b.ts"),
            ("a/./b//c.ts", "a/b/c.ts"),
            ("../src/a.ts", "../src/a.ts"),
            ("a/../../b.ts", "../b.ts"),
            ("/abs/./x/../y.ts", "/abs/y.ts"),
            ("/../y.ts", "/y.ts"),
            ("webpack:///./src/app.ts", "webpack:///src/app.ts"),
            (
                "webpack://app/./src/../index.ts?a#b",
                "webpack://app/index.ts?a#b",
            ),
            (
                "https://user@h.test:8080/a/../b.js",
                "https://user@h.test:8080/b.js",
            ),
            ("https://h.test", "https://h.test/"),
            ("//cdn.test/a/./b.js", "//cdn.test/a/b.js"),
            ("file:///C:/a/../b.ts", "file:///C:/b.ts"),
            ("file://C:/a.ts", "file:///C:/a.ts"),
            ("file://host/a.ts", "file://host/a.ts"),
            ("file:a.ts", "file:///a.ts"),
            ("a/", "a/"),
            ("a/..", "."),
            ("a/b/..", "a/"),
            ("?q#h", "?q#h"),
            ("#h", "#h"),
            (".", "."),
            ("", ""),
            ("/a?x#y", "/a?x#y"),
            ("/a#y?x", "/a#y?x"),
            ("a?x", "a?x"),
            ("./", "."),
            ("..", ".."),
            ("../", ".."),
            // Only a lowercase "file:" is read as a file URL.
            ("FILE:///x", "FILE:///x"),
            ("file:///a/b?q#h", "file:///a/b?q#h"),
            ("a:b/c.ts", "a:b/c.ts"),
            ("//h", "//h/"),
            ("http://h/a/..", "http://h/"),
            ("x/#frag", "x/#frag"),
            // A port that is not a number ends the URL for the library.
            ("https://h.test:abc/x", "https://h.test/"),
            ("//h:abc/x", "//h/"),
            ("https://h.test?q", "https://h.test/?q"),
        ];
        for (input, expected) in cases {
            assert_eq!(resolve_uri(input), expected, "{input}");
        }
        let tm = TraceMap::new(&json!({
            "sourceRoot": "webpack://app/src",
            "sources": ["./a.ts", null],
            "mappings": "AAAA"
        }))
        .unwrap();
        assert_eq!(
            tm.resolved_sources,
            ["webpack://app/src/a.ts", "webpack://app/src/"]
        );
        assert_eq!(tm.sources, [json!("./a.ts"), Value::Null]);
    }

    #[test]
    fn 오프셋은_utf16_단위로_줄과_열이_된다() {
        assert_eq!(offset_to_line_col("ab\ncd", 0), (0, 0));
        assert_eq!(offset_to_line_col("ab\ncd", 2), (0, 2));
        assert_eq!(offset_to_line_col("ab\ncd", 4), (1, 1));
        assert_eq!(offset_to_line_col("ab\ncd", 99), (1, 2));
        // "😀" is two units, "한" one.
        assert_eq!(offset_to_line_col("한😀x", 3), (0, 3));
    }

    #[test]
    fn data_url은_base64와_퍼센트_인코딩을_모두_읽는다() {
        let b64 = format!(
            "data:application/json;base64,{}",
            STANDARD.encode("{\"a\":\"한\"}")
        );
        assert_eq!(data_url_json(&b64), Ok("{\"a\":\"한\"}".to_owned()));
        assert_eq!(
            data_url_json("data:application/json,%7B%22a%22%3A%22%ED%95%9C%22%7D"),
            Ok("{\"a\":\"한\"}".to_owned())
        );
        assert_eq!(data_url_json("data:application/json"), Ok(String::new()));
        // "base64," with nothing after it falls through to the comma form.
        assert_eq!(data_url_json("data:x;base64,"), Ok(String::new()));
        for bad in ["data:,%", "data:,%4", "data:,%zz", "data:,%ff"] {
            assert_eq!(data_url_json(bad), Err("URI malformed".to_owned()), "{bad}");
        }
    }

    #[tokio::test]
    async fn 인라인_소스맵으로_줄과_열을_원본_위치와_스니펫으로_바꾼다() {
        let ctx = context(vec![inline("s1", "a();\n    sign(key);", MAP)]);
        assert_eq!(
            call(
                ResolveSource,
                &ctx,
                json!({ "requestId": "s1", "line": 1, "column": 4 })
            )
            .await,
            (
                false,
                r#"{
  "source": "webpack:///src/app.ts",
  "line": 11,
  "column": 2,
  "name": "sign",
  "snippet": "l7\nl8\nl9\nl10\nl11\nl12\nl13\nl14\nl15\nl16"
}"#
                .to_owned()
            )
        );
        // byteOffset wins over line/column: offset 9 is line 1, column 4.
        let by_offset = call(
            ResolveSource,
            &ctx,
            json!({ "requestId": "s1", "byteOffset": 9, "line": 0, "column": 0 }),
        )
        .await;
        assert!(
            by_offset.1.contains("\"name\": \"sign\""),
            "{}",
            by_offset.1
        );
    }

    #[tokio::test]
    async fn 매핑이_없는_위치와_본문이_없는_소스는_null과_스니펫_없이_돌려준다() {
        let ctx = context(vec![inline("s1", "a();\n    sign(key);", MAP)]);
        assert_eq!(
            call(ResolveSource, &ctx, json!({ "requestId": "s1", "line": 2 })).await,
            (
                false,
                "{\n  \"source\": null,\n  \"line\": null,\n  \"column\": null,\n  \"name\": null\n}"
                    .to_owned()
            )
        );
        // Defaults are line 0, column 0; lib/util.ts has null sourcesContent.
        assert_eq!(
            call(ResolveSource, &ctx, json!({ "requestId": "s1", "line": 3 })).await,
            (
                false,
                "{\n  \"source\": \"lib/util.ts\",\n  \"line\": 1,\n  \"column\": 0,\n  \"name\": null\n}"
                    .to_owned()
            )
        );
    }

    #[tokio::test]
    async fn 소스_목록은_맵에_적힌_그대로이고_원본은_두_이름_모두로_찾는다() {
        let ctx = context(vec![inline("s1", "a();", MAP)]);
        assert_eq!(
            call(ListSources, &ctx, json!({ "requestId": "s1" })).await,
            (
                false,
                "{\n  \"sources\": [\n    \"webpack:///./src/app.ts\",\n    \"lib/util.ts\"\n  ]\n}"
                    .to_owned()
            )
        );
        for name in ["webpack:///./src/app.ts", "webpack:///src/app.ts"] {
            let (is_error, text) = call(
                GetOriginalSource,
                &ctx,
                json!({ "requestId": "s1", "source": name }),
            )
            .await;
            assert!(!is_error);
            assert!(
                text.starts_with("l1\nl2\n") && text.ends_with("\nl17"),
                "{text}"
            );
        }
        for name in ["lib/util.ts", "missing.ts"] {
            assert_eq!(
                call(
                    GetOriginalSource,
                    &ctx,
                    json!({ "requestId": "s1", "source": name })
                )
                .await,
                (true, format!("no sourcesContent for {name}"))
            );
        }
        assert!(
            call(GetOriginalSource, &ctx, json!({ "requestId": "s1" }))
                .await
                .0
        );
    }

    #[tokio::test]
    async fn 맵을_찾을_수_없는_이유를_알린다() {
        let ctx = context(vec![
            script("plain", "http://h/a.js", "a();"),
            script("empty", "http://h/b.js", ""),
            RequestPatch {
                response_body_base64: Some(true),
                ..script("binary", "http://h/c.js", "AAAA")
            },
            script(
                "badjson",
                "http://h/d.js",
                "//# sourceMappingURL=data:application/json,{oops",
            ),
            script(
                "notmap",
                "http://h/e.js",
                "//@ sourceMappingURL=data:application/json,%7B%7D",
            ),
            script(
                "nocomma",
                "http://h/f.js",
                "//# sourceMappingURL=data:application/json",
            ),
            script(
                "baduri",
                "http://h/g.js",
                "//# sourceMappingURL=data:application/json,%zz",
            ),
            script("badbase", "not a url", "//# sourceMappingURL=app.js.map"),
        ]);
        let error = |id: &'static str| {
            let ctx = &ctx;
            async move { call(ListSources, ctx, json!({ "requestId": id })).await }
        };
        assert_eq!(
            error("nope").await,
            (true, "unknown requestId: nope".into())
        );
        assert_eq!(
            error("empty").await,
            (true, "no response body for empty".into())
        );
        assert_eq!(
            error("binary").await,
            (true, "body is base64 (binary) for binary".into())
        );
        assert_eq!(
            error("plain").await,
            (true, "no sourceMappingURL found in plain".into())
        );
        assert_eq!(
            error("notmap").await,
            (
                true,
                "failed to parse source map: invalid source map: {}".into()
            )
        );
        assert_eq!(error("baduri").await, (true, "URI malformed".into()));
        assert_eq!(
            error("badbase").await,
            (true, "cannot resolve source map URL: app.js.map".into())
        );
        for id in ["badjson", "nocomma"] {
            let (is_error, text) = error(id).await;
            assert!(is_error);
            assert!(text.starts_with("failed to parse source map: "), "{text}");
        }
    }

    #[tokio::test]
    async fn 외부_소스맵은_스크립트_url_기준으로_받아오고_한_번만_받는다() {
        let base = serve_once(
            "200 OK",
            "\u{feff}{\"version\":3,\"sources\":[\"ext.ts\"],\"mappings\":\"AAAA\"}",
        );
        let ctx = context(vec![script(
            "s1",
            &format!("{base}/static/js/app.js"),
            "a();\n//# sourceMappingURL=../maps/app.js.map",
        )]);
        let expected = (
            false,
            "{\n  \"sources\": [\n    \"ext.ts\"\n  ]\n}".to_owned(),
        );
        assert_eq!(
            call(ListSources, &ctx, json!({ "requestId": "s1" })).await,
            expected
        );
        // The server answered its one request and is gone: this is the cache.
        assert_eq!(
            call(ListSources, &ctx, json!({ "requestId": "s1" })).await,
            expected
        );
    }

    #[tokio::test]
    async fn 외부_소스맵을_받지_못하면_상태_코드나_이유를_알린다() {
        let base = serve_once("404 Not Found", "nope");
        let ctx = context(vec![
            script(
                "missing",
                "http://h/app.js",
                &format!("//# sourceMappingURL={base}/app.js.map"),
            ),
            // Nothing listens on port 1.
            script(
                "down",
                "http://127.0.0.1:1/app.js",
                "//# sourceMappingURL=app.js.map",
            ),
        ]);
        assert_eq!(
            call(ListSources, &ctx, json!({ "requestId": "missing" })).await,
            (
                true,
                format!("failed to fetch source map {base}/app.js.map: 404")
            )
        );
        let (is_error, text) = call(ListSources, &ctx, json!({ "requestId": "down" })).await;
        assert!(is_error);
        assert!(text.starts_with("fetch source map failed: "), "{text}");
        // A failed load is not cached.
        assert!(
            ctx.state::<SourceMapCache>()
                .maps
                .lock()
                .unwrap()
                .is_empty()
        );
    }
}
