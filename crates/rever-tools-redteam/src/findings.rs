//! Port of `src/main/mcp/tools/findings.ts` — the `finding_*` tools, the
//! findings store behind them and the Markdown report.
//!
//! Evidence → Finding → Path: a finding is a claim; it stays `candidate` until it
//! carries enough independent evidence to be `validated`.
//!
//! A stored finding is kept as the JSON object it is on disk rather than as a
//! typed struct: `findings.json` is written back whole on every change, so an
//! entry with unknown or oddly typed fields must survive the round trip, in the
//! key order the Electron app wrote.

use std::cmp::Ordering;
use std::path::Path;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::{Captures, Regex};
use rever_scope::findings_dedupe::{
    DedupableFinding, MergeResult, endpoint_key_of, is_scan_noise, merge_duplicate_endpoints,
};
use rever_scope::findings_scope::{
    ExportScope, HasOrigin, canonical_origin_key, filter_by_origin, origin_of, resolve_export_scope,
};
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::args::{strings, text, validate};
use crate::robots::RobotsNotes;
use crate::{active_url, pretty, schema};

pub fn register(registry: &mut ToolRegistry) {
    registry.register(FindingAdd);
    registry.register(FindingList);
    registry.register(FindingExport);
    registry.register(FindingRemove);
}

/// One stored finding: `id`, `title`, `severity`, `category`, `body`,
/// `requestId`, `evidenceIds` (requestIds and/or other finding ids that back the
/// claim), `reproCommand`, `status`, `tags`, `origin` (the site the finding was
/// recorded on; scopes the export report) and `createdAt` (epoch milliseconds).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Finding(pub Map<String, Value>);

impl Finding {
    /// A string field; empty when missing or not a string.
    pub fn text(&self, key: &str) -> &str {
        self.0.get(key).and_then(Value::as_str).unwrap_or_default()
    }

    /// The strings of an array field.
    fn list(&self, key: &str) -> Vec<String> {
        let items = self.0.get(key).and_then(Value::as_array);
        let strings = items.into_iter().flatten().filter_map(Value::as_str);
        strings.map(str::to_owned).collect()
    }

    fn created_at(&self) -> f64 {
        self.0
            .get("createdAt")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    }

    /// The typed view the dedupe logic and the report work on. Fields it does
    /// not name (`reproCommand`, `origin`, …) ride along in `extra`.
    fn view(&self) -> DedupableFinding {
        const TYPED: [&str; 10] = [
            "id",
            "title",
            "body",
            "category",
            "severity",
            "status",
            "tags",
            "evidenceIds",
            "requestId",
            "createdAt",
        ];
        let field = |key: &str| self.text(key).to_owned();
        DedupableFinding {
            id: field("id"),
            title: field("title"),
            body: field("body"),
            category: field("category"),
            severity: field("severity"),
            status: field("status"),
            tags: self.list("tags"),
            evidence_ids: self.list("evidenceIds"),
            request_id: self
                .0
                .get("requestId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            created_at: self.created_at() as i64,
            extra: self
                .0
                .iter()
                .filter(|(key, _)| !TYPED.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        }
    }
}

impl HasOrigin for Finding {
    fn origin(&self) -> Option<&str> {
        self.0.get("origin").and_then(Value::as_str)
    }
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 5,
        "high" => 4,
        "medium" => 3,
        "low" => 2,
        "info" => 1,
        // The TypeScript comparator yields NaN for a severity outside the enum
        // (only a hand-edited store has one); ranking it last keeps the order total.
        _ => 0,
    }
}

/// `None` for a value outside the enum (only a hand-edited store has one): the
/// TypeScript's table lookup is `undefined` there, which prints differently in
/// the two places the label is used.
fn severity_label(severity: &str) -> Option<&'static str> {
    Some(match severity {
        "critical" => "🔴 critical",
        "high" => "🟠 high",
        "medium" => "🟡 medium",
        "low" => "🟢 low",
        "info" => "ℹ️ info",
        _ => return None,
    })
}

fn category_label(category: &str) -> Option<&'static str> {
    Some(match category {
        "endpoint" => "API Endpoints",
        "auth" => "Auth Flow",
        "vuln" => "Vulnerabilities",
        "secret" => "Secrets / Keys",
        "other" => "Other",
        _ => return None,
    })
}

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([A-Za-z0-9._%+-]{1,2})[A-Za-z0-9._%+-]*(@[A-Za-z0-9.-]+\.[A-Za-z]{2,})")
        .expect("built-in pattern compiles")
});

// `\b0\d{1,2}[-\s]?\d{3,4}[-\s]?\d{4}\b` with JavaScript's meaning of the
// classes: `\b` and `\d` are ASCII-only there (so a phone number glued to Hangul
// is still found), and `\s` is JavaScript's whitespace set, which has U+FEFF and
// lacks U+0085.
static PHONE: LazyLock<Regex> = LazyLock::new(|| {
    const SEP: &str = r"[-\t\n\x0B\x0C\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";
    Regex::new(&format!(
        r"(?-u:\b)0[0-9]{{1,2}}{SEP}?[0-9]{{3,4}}{SEP}?[0-9]{{4}}(?-u:\b)"
    ))
    .expect("built-in pattern compiles")
});

/// Redact PII before a finding is stored or rendered. The body is free text a
/// worker wrote and workers mask inconsistently (one scan masked an applicant
/// email, the next pasted it raw into the report), so enforce it deterministically.
/// Emails and KR phone numbers only — a Korean name can't be told from ordinary
/// Korean prose without mangling the whole report. Idempotent: already-masked
/// values carry no digits/`@`-run to re-hit.
pub fn mask_pii(text: &str) -> String {
    let emails_masked = EMAIL.replace_all(text, "${1}****${2}");
    PHONE
        .replace_all(&emails_masked, |found: &Captures| {
            let number = found.get(0).map_or("", |m| m.as_str());
            // `/^0\d{1,2}/`: the leading 0 and up to two more digits.
            let digits = number.bytes().take(3).take_while(u8::is_ascii_digit);
            let prefix = number.get(..digits.count()).unwrap_or(number);
            format!("{prefix}-****-****")
        })
        .into_owned()
}

/// `String.prototype.trim()`: JavaScript's whitespace has U+FEFF and lacks U+0085.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c == '\u{feff}' || (c.is_whitespace() && c != '\u{85}'))
}

const STORE_FILE: &str = "findings.json";

/// The findings of this app, loaded from `<data_dir>/findings.json` on first
/// use, in the order they were first stored. Held in `ctx.state::<FindingStore>()`.
#[derive(Default)]
pub struct FindingStore(Mutex<StoreState>);

#[derive(Default)]
struct StoreState {
    loaded: bool,
    findings: Vec<Finding>,
}

/// JSON.stringify writes half of a surrogate pair as an escape (`\ud800`) and
/// JSON.parse reads it back, but serde_json rejects the whole file for it — and
/// a store read as empty is saved over every earlier finding by the next
/// change. A Rust string cannot hold the half, so it becomes U+FFFD.
fn without_lone_surrogates(text: &str) -> String {
    // The UTF-16 unit a `\uXXXX` escape at the start of `s` stands for.
    let unit = |s: &str| u32::from_str_radix(s.strip_prefix("\\u")?.get(..4)?, 16).ok();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, escape)) = rest.find('\\').and_then(|at| rest.split_at_checked(at)) {
        out.push_str(before);
        let first = unit(escape);
        let paired = first.is_some_and(|u| (0xD800..0xDC00).contains(&u))
            && escape
                .get(6..)
                .and_then(unit)
                .is_some_and(|u| (0xDC00..0xE000).contains(&u));
        let len = if paired {
            12
        } else if first.is_some_and(|u| (0xD800..0xE000).contains(&u)) {
            out.push_str("\\ufffd");
            rest = escape.get(6..).unwrap_or_default();
            continue;
        } else {
            // Any other escape goes through with the character it escapes, so
            // the `u` after an escaped backslash (`\\ud800`) is not read as one.
            1 + escape.chars().nth(1).map_or(0, char::len_utf8)
        };
        let (kept, after) = escape.split_at_checked(len).unwrap_or((escape, ""));
        out.push_str(kept);
        rest = after;
    }
    out.push_str(rest);
    out
}

fn load(path: &Path) -> Vec<Finding> {
    let mut findings: Vec<Finding> = Vec::new();
    if !path.exists() {
        return findings;
    }
    // Lossy, like Node's 'utf8' decoding: a stray byte must not empty the store.
    let parsed = std::fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|bytes| {
            let text = String::from_utf8_lossy(&bytes);
            serde_json::from_str::<Vec<Value>>(&text)
                .or_else(|_| serde_json::from_str(&without_lone_surrogates(&text)))
                .map_err(|e| e.to_string())
        });
    let entries = match parsed {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("[findings] load failed: {error}");
            return findings;
        }
    };
    for entry in entries {
        let Value::Object(mut f) = entry else {
            continue;
        };
        // Tolerate findings written before `category` / the evidence chain existed.
        let defaults = [
            ("category", json!("other")),
            ("tags", json!([])),
            ("evidenceIds", json!([])),
            ("status", json!("candidate")),
        ];
        for (key, default) in defaults {
            if f.get(key).is_none_or(Value::is_null) {
                f.insert(key.to_owned(), default);
            }
        }
        // Heal origins stamped before loopback spellings were folded (a
        // "127.0.0.1:1337" stamp made the finding invisible to a
        // "localhost:1337" export).
        match canonical_origin_key(f.get("origin").and_then(Value::as_str)) {
            Some(origin) => f.insert("origin".to_owned(), json!(origin)),
            None => f.shift_remove("origin"),
        };
        // A repeated id replaces the earlier entry in place, as Map.set does.
        match findings.iter_mut().find(|e| e.0.get("id") == f.get("id")) {
            Some(slot) => slot.0 = f,
            None => findings.push(Finding(f)),
        }
    }
    findings
}

fn save(path: &Path, findings: &[Finding]) {
    let written = serde_json::to_string_pretty(findings)
        .map_err(|e| e.to_string())
        .and_then(|text| {
            // Electron's userData folder always exists; a fresh data_dir may not.
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(path, text).map_err(|e| e.to_string())
        });
    if let Err(error) = written {
        eprintln!("[findings] save failed: {error}");
    }
}

/// Runs `f` on the loaded store, one caller at a time. `f` returns its result
/// and whether the store changed; a changed store is written back before the
/// next caller gets in.
fn with_store<R>(ctx: &ToolContext, f: impl FnOnce(&mut Vec<Finding>) -> (R, bool)) -> R {
    let path = ctx.data_dir.join(STORE_FILE);
    let store = ctx.state::<FindingStore>();
    let mut state = store.0.lock().unwrap_or_else(PoisonError::into_inner);
    if !state.loaded {
        state.loaded = true;
        state.findings = load(&path);
    }
    let (result, changed) = f(&mut state.findings);
    if changed {
        save(&path, &state.findings);
    }
    result
}

/// Newest first; findings of the same millisecond keep their stored order.
fn newest_first(findings: &[Finding]) -> Vec<Finding> {
    let mut list = findings.to_vec();
    list.sort_by(|a, b| b.created_at().total_cmp(&a.created_at()));
    list
}

pub fn list_findings(ctx: &ToolContext) -> Vec<Finding> {
    with_store(ctx, |findings| (newest_first(findings), false))
}

/// A rescan of the same site must not inherit the previous app's findings —
/// the origin scope (eTLD+1) cannot tell two apps apart when e.g. a new
/// challenge is deployed on the same localhost port.
pub fn clear_findings_for_site(ctx: &ToolContext, url: &str) -> usize {
    with_store(ctx, |findings| {
        let Some(scope) = origin_of(Some(url)) else {
            return (0, false);
        };
        let before = findings.len();
        findings.retain(|f| f.origin() != Some(scope.as_str()));
        let removed = before - findings.len();
        (removed, removed > 0)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportBucket {
    Confirmed,
    Candidate,
    Recon,
}

/// Where a finding belongs in the report. A read-only, capture-not-send scan
/// can rarely *prove* a claim, so most findings are suspicions, not facts —
/// presenting them all as "findings" is what makes the report read as noise.
/// - recon: endpoint inventory, info-level notes, and accepted-risk items are
///   attack-surface mapping, never headline vulns.
/// - confirmed: only `validated` (≥2 independent evidence) issues are stated as fact.
/// - candidate: everything else is unproven and must be verified before acting.
pub fn report_bucket(category: &str, severity: &str, status: &str) -> ReportBucket {
    if category == "endpoint" || severity == "info" || status == "accepted_risk" {
        return ReportBucket::Recon;
    }
    if status == "validated" {
        ReportBucket::Confirmed
    } else {
        ReportBucket::Candidate
    }
}

/// `SEVERITY_RANK[b] - SEVERITY_RANK[a] || b.createdAt - a.createdAt`
fn by_severity(a: &DedupableFinding, b: &DedupableFinding) -> Ordering {
    severity_rank(&b.severity)
        .cmp(&severity_rank(&a.severity))
        .then(b.created_at.cmp(&a.created_at))
}

/// Whether the finding's own URL sits on a robots.txt-disallowed path.
fn robots_disallows(f: &DedupableFinding, robots: &RobotsNotes) -> bool {
    f.extra
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|url| robots.disallows(url))
}

fn render_detail(f: &DedupableFinding, robots: &RobotsNotes, lines: &mut Vec<String>) {
    // Backstop: findings stored before masking (or imported) are masked here too.
    lines.push(format!("### {}", mask_pii(&f.title)));
    // Array.prototype.join prints an undefined label as nothing.
    let mut meta = vec![
        severity_label(&f.severity).unwrap_or_default().to_owned(),
        category_label(&f.category).unwrap_or_default().to_owned(),
        format!("status: {}", f.status),
    ];
    if robots_disallows(f, robots) {
        meta.push("robots: disallowed path".to_owned());
    }
    if !f.tags.is_empty() {
        meta.push(format!("tags: {}", f.tags.join(", ")));
    }
    if let Some(request_id) = f.request_id.as_deref().filter(|id| !id.is_empty()) {
        meta.push(format!("request: `{request_id}`"));
    }
    lines.push(format!("_{}_", meta.join(" · ")));
    lines.push(String::new());
    lines.push(js_trim(&mask_pii(&f.body)).to_owned());
    lines.push(String::new());
    if !f.evidence_ids.is_empty() {
        let evidence: Vec<String> = f.evidence_ids.iter().map(|e| format!("`{e}`")).collect();
        lines.push(format!("**Evidence:** {}", evidence.join(", ")));
        lines.push(String::new());
    }
    let repro = f.extra.get("reproCommand").and_then(Value::as_str);
    if let Some(repro) = repro.filter(|r| !r.is_empty()) {
        lines.push("**Repro:**".to_owned());
        lines.push("```".to_owned());
        lines.push(js_trim(&mask_pii(repro)).to_owned());
        lines.push("```".to_owned());
        lines.push(String::new());
    }
}

fn render_markdown(all: &[Finding], scope_note: Option<&str>, robots: &RobotsNotes) -> String {
    // Parallel tab workers each record the same public endpoint; the export is
    // the one place that can merge them deterministically, whatever the workers
    // did. (finding_add already nudges duplicates away at the source.)
    // Legacy guard: drop any negative/false-positive noise already on disk so an
    // old scan's report is clean without a rescan (finding_add blocks new ones).
    let kept = all.iter().map(Finding::view);
    let kept: Vec<DedupableFinding> = kept
        .filter(|f| !is_scan_noise(&f.status, &f.tags))
        .collect();
    let MergeResult {
        merged,
        merged_away,
    } = merge_duplicate_endpoints(kept);
    let mut lines: Vec<String> = vec!["# Reversing Session Findings".to_owned(), String::new()];
    if let Some(note) = scope_note.filter(|note| !note.is_empty()) {
        lines.push(format!("_site: {note}_"));
        lines.push(String::new());
    }

    let (mut confirmed, mut candidate, mut recon) = (Vec::new(), Vec::new(), Vec::new());
    for f in merged {
        match report_bucket(&f.category, &f.severity, &f.status) {
            ReportBucket::Confirmed => confirmed.push(f),
            ReportBucket::Candidate => candidate.push(f),
            ReportBucket::Recon => recon.push(f),
        }
    }

    // Honest glance: confirmed facts vs suspicions vs inventory — not a flat count.
    let merged_note = match merged_away {
        0 => String::new(),
        1 => " _(1 duplicate report merged)_".to_owned(),
        n => format!(" _({n} duplicate reports merged)_"),
    };
    lines.push(format!(
        "**{} confirmed · {} to verify · {} recon**{merged_note}",
        confirmed.len(),
        candidate.len(),
        recon.len()
    ));
    lines.push(String::new());

    if !confirmed.is_empty() {
        lines.push(format!("## Confirmed ({})", confirmed.len()));
        lines.push(String::new());
        confirmed.sort_by(by_severity);
        for f in &confirmed {
            render_detail(f, robots, &mut lines);
        }
    }
    if !candidate.is_empty() {
        lines.push(format!("## Needs verification ({})", candidate.len()));
        lines.push(String::new());
        lines.push("_Unproven — this read-only scan could not confirm these. Check the code or a live request before acting; absence of a client-side control is not proof of a server-side one._".to_owned());
        lines.push(String::new());
        candidate.sort_by(by_severity);
        for f in &candidate {
            render_detail(f, robots, &mut lines);
        }
    }
    if !recon.is_empty() {
        // Inventory, not vulnerabilities — one compact line each so it doesn't bury the findings above.
        lines.push(format!("## Attack surface & recon ({})", recon.len()));
        lines.push(String::new());
        recon.sort_by(by_severity);
        for f in &recon {
            let evidence = match f.evidence_ids.first().filter(|e| !e.is_empty()) {
                Some(first) => format!(" · `{first}`"),
                None => String::new(),
            };
            // A robots-disallowed endpoint is inventory the site itself
            // declared off-limits: worth naming where the attack surface is
            // listed, without inflating it into a vulnerability.
            let robots_mark = if robots_disallows(f, robots) {
                " · robots-disallowed"
            } else {
                ""
            };
            // A template literal prints an undefined label as "undefined".
            lines.push(format!(
                "- **{}** _({}, {})_{evidence}{robots_mark}",
                mask_pii(&f.title),
                severity_label(&f.severity).unwrap_or("undefined"),
                category_label(&f.category).unwrap_or("undefined")
            ));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

/// `Date.now()`
fn now_ms() -> u64 {
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH);
    since_epoch.map_or(0, |elapsed| elapsed.as_millis() as u64)
}

pub struct FindingAdd;

impl Tool for FindingAdd {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "finding_add",
            "Save a finding/evidence note as a durable session artifact (persisted to disk). Use during reversing to bookmark endpoints, auth flows, secrets, or vulnerabilities. Markdown body supported. Back the claim with `evidenceIds` (requestIds and/or other finding ids) and a `reproCommand`; a finding stays `candidate` until it carries ≥2 independent evidence items (best: 1 static + 1 dynamic), at which point you may mark it `validated`.",
            schema(
                json!({
                    "title": {
                        "type": "string",
                        "description": "Short title"
                    },
                    "body": {
                        "type": "string",
                        "description": "Markdown body — observations, payload, repro, etc."
                    },
                    "url": {
                        "type": "string",
                        "description": "URL of the affected endpoint or page. Stored on the finding so the report can annotate robots.txt status"
                    },
                    "category": {
                        "description": "What kind of finding this is (groups the export report)",
                        "type": "string",
                        "enum": ["endpoint", "auth", "vuln", "secret", "other"]
                    },
                    "severity": {
                        "type": "string",
                        "enum": ["info", "low", "medium", "high", "critical"]
                    },
                    "requestId": {
                        "description": "Optional traffic-store requestId reference",
                        "type": "string"
                    },
                    "evidenceIds": {
                        "description": "requestIds and/or other finding ids that back this claim (the evidence chain)",
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "reproCommand": {
                        "description": "Exact command/script that reproduces the claim (e.g. an export_python_client path)",
                        "type": "string"
                    },
                    "status": {
                        "description": "Defaults to candidate. `validated` requires ≥2 independent evidence items.",
                        "type": "string",
                        "enum": ["candidate", "validated", "false_positive", "accepted_risk"]
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }),
                &["title", "body"],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            let title = text(&args, "title").unwrap_or_default();
            let body = text(&args, "body").unwrap_or_default();
            let url = text(&args, "url").filter(|url| !url.is_empty());
            let category = text(&args, "category").unwrap_or("other");
            let severity = text(&args, "severity").unwrap_or("info");
            let request_id = text(&args, "requestId");
            let evidence_ids = strings(&args, "evidenceIds").unwrap_or_default();
            let repro_command = text(&args, "reproCommand").filter(|repro| !repro.is_empty());
            let status = text(&args, "status").unwrap_or("candidate");
            let tags = strings(&args, "tags").unwrap_or_default();

            // Asked before the store is locked: the duplicate check and the insert
            // below must be one step, or two parallel tab workers both pass the check.
            let origin = origin_of(active_url(ctx).await.as_deref());

            with_store(ctx, |findings| {
                // Duplicate gate: parallel tab workers each see the same public endpoint
                // and would otherwise record it once per tab. Point the caller at the
                // existing finding instead of storing another copy.
                let key = endpoint_key_of(&DedupableFinding {
                    title: title.to_owned(),
                    body: body.to_owned(),
                    category: category.to_owned(),
                    ..DedupableFinding::default()
                });
                if let Some(key) = key {
                    let stored = newest_first(findings).into_iter().map(|f| f.view());
                    let dupe = stored
                        .into_iter()
                        .find(|f| endpoint_key_of(f).as_ref() == Some(&key));
                    if let Some(dupe) = dupe {
                        let refusal = format!(
                            "not saved: an endpoint finding for the same host+path already exists — #{} \"{}\" ({}, {}).\n\
                             If this adds new evidence, record it on that finding instead (its body/evidence), or finding_remove it first.\n\
                             If this is a genuinely different endpoint, include the full URL in the title.",
                            dupe.id, dupe.title, dupe.severity, dupe.status
                        );
                        return (ok(refusal), false);
                    }
                }
                // Validated gate: never silently promote a thinly-evidenced claim.
                let mut effective_status = status;
                let mut warning = None;
                let evidence =
                    evidence_ids.len() + usize::from(request_id.is_some_and(|id| !id.is_empty()));
                if status == "validated" && evidence < 2 {
                    effective_status = "candidate";
                    warning = Some(format!(
                        "status downgraded to 'candidate': 'validated' needs ≥2 independent evidence items, got {evidence}. Add more evidenceIds (best: 1 static + 1 dynamic) or record it as residual risk."
                    ));
                }
                // Noise gate: a pure-negative result is not recorded (scan-noise policy).
                // Workers flag their own negatives; don't let them flood the report.
                if is_scan_noise(effective_status, &tags) {
                    let refusal = "not saved: negative / false-positive results aren't recorded (scan-noise policy) — \
                                   the report carries only confirmed or candidate vulnerabilities. \
                                   If you ruled OUT a vuln that was already saved, finding_remove it instead.";
                    return (ok(refusal), false);
                }
                let id = Uuid::new_v4().to_string();
                let mut f = Map::new();
                f.insert("id".into(), json!(id));
                // Mask PII at the source so it never lands on disk, not just in the report.
                f.insert("title".into(), json!(mask_pii(title)));
                f.insert("severity".into(), json!(severity));
                f.insert("category".into(), json!(category));
                f.insert("body".into(), json!(mask_pii(body)));
                if let Some(url) = url {
                    f.insert("url".into(), json!(mask_pii(url)));
                }
                if let Some(request_id) = request_id {
                    f.insert("requestId".into(), json!(request_id));
                }
                f.insert("evidenceIds".into(), json!(evidence_ids));
                if let Some(repro_command) = repro_command {
                    f.insert("reproCommand".into(), json!(mask_pii(repro_command)));
                }
                f.insert("status".into(), json!(effective_status));
                f.insert("tags".into(), json!(tags));
                if let Some(origin) = origin {
                    f.insert("origin".into(), json!(origin));
                }
                f.insert("createdAt".into(), json!(now_ms()));
                findings.push(Finding(f));

                let mut saved = Map::new();
                saved.insert("id".into(), json!(id));
                saved.insert("title".into(), json!(title));
                saved.insert("category".into(), json!(category));
                saved.insert("severity".into(), json!(severity));
                saved.insert("status".into(), json!(effective_status));
                saved.insert("evidenceCount".into(), json!(evidence));
                if let Some(warning) = warning {
                    saved.insert("warning".into(), json!(warning));
                }
                (ok(pretty(&saved)), true)
            })
        })
    }
}

pub struct FindingList;

impl Tool for FindingList {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "finding_list",
            "List saved findings (newest first).",
            schema(
                json!({
                    "category": {
                        "type": "string",
                        "enum": ["endpoint", "auth", "vuln", "secret", "other"]
                    },
                    "tag": {
                        "description": "Filter by tag",
                        "type": "string"
                    },
                    "severity": {
                        "type": "string",
                        "enum": ["info", "low", "medium", "high", "critical"]
                    }
                }),
                &[],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            // An empty string filters nothing, as it is falsy in the TypeScript.
            let filter = |key: &str| text(&args, key).filter(|value| !value.is_empty());
            let mut list = list_findings(ctx);
            if let Some(category) = filter("category") {
                list.retain(|f| f.0.get("category").and_then(Value::as_str) == Some(category));
            }
            if let Some(tag) = filter("tag") {
                list.retain(|f| {
                    let tags = f.0.get("tags").and_then(Value::as_array);
                    tags.is_some_and(|tags| tags.iter().any(|t| t.as_str() == Some(tag)))
                });
            }
            if let Some(severity) = filter("severity") {
                list.retain(|f| f.0.get("severity").and_then(Value::as_str) == Some(severity));
            }
            ok(pretty(&list))
        })
    }
}

pub struct FindingExport;

impl Tool for FindingExport {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "finding_export",
            "Render findings as a structured Markdown report — the session deliverable. Split into Confirmed (validated facts), Needs verification (unproven suspicions to check before acting), and Attack surface & recon (endpoint inventory, compact). Defaults to the current page's site so one scan's report is not polluted by findings from other targets; pass site=<url> when no tab is active (e.g. from a finished scan), or all=true for every finding in the store.",
            schema(
                json!({
                    "site": {
                        "description": "URL of the scanned site — use when the page/tab is gone so the report stays scoped",
                        "type": "string"
                    },
                    "all": {
                        "description": "Include findings from every site, not just the current page's",
                        "type": "boolean"
                    }
                }),
                &[],
            ),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
            let active = active_url(ctx).await;
            let ExportScope { scope, error } =
                resolve_export_scope(text(&args, "site"), active.as_deref(), all);
            if let Some(error) = error.filter(|error| !error.is_empty()) {
                return err(error);
            }
            let list = filter_by_origin(list_findings(ctx), scope.as_deref(), all);
            let scope = scope.filter(|scope| !scope.is_empty());
            if list.is_empty() {
                return ok(match scope {
                    Some(scope) => format!("No findings recorded yet for {scope}."),
                    None => "No findings recorded yet.".to_owned(),
                });
            }
            ok(render_markdown(
                &list,
                scope.as_deref().filter(|_| !all),
                &RobotsNotes::from_traffic(ctx),
            ))
        })
    }
}

pub struct FindingRemove;

impl Tool for FindingRemove {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "finding_remove",
            "Delete a finding by ID.",
            schema(json!({ "id": { "type": "string" } }), &["id"]),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(refusal) = validate(&self.def(), &args) {
                return refusal;
            }
            let id = text(&args, "id").unwrap_or_default();
            with_store(ctx, |findings| {
                let before = findings.len();
                findings.retain(|f| f.0.get("id").and_then(Value::as_str) != Some(id));
                if findings.len() == before {
                    return (err(format!("unknown finding: {id}")), false);
                }
                (ok(format!("removed {id}")), true)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::testing::{Page, TempDir, call};

    // --- ported from findings.test.ts ---

    // The report is a deliverable; applicant PII must never reach it. One scan
    // pasted real emails/phones into the body (regression), so masking is enforced
    // deterministically rather than left to the worker.

    #[test]
    fn 이메일은_앞_2글자와_도메인만_남긴다_실제_유출값() {
        assert_eq!(
            mask_pii("email rnjstnfls00@naver.com here"),
            "email rn****@naver.com here"
        );
        assert_eq!(mask_pii("greekr3@nate.com"), "gr****@nate.com");
    }

    #[test]
    fn 한국_전화번호를_가린다_하이픈_무하이픈_지역번호() {
        assert_eq!(mask_pii("phone 010-1234-5678"), "phone 010-****-****");
        assert_eq!(mask_pii("01012345678"), "010-****-****");
        assert_eq!(mask_pii("02-123-4567"), "02-****-****");
    }

    #[test]
    fn 이미_가려진_값은_다시_건드리지_않는다_멱등() {
        let once = mask_pii("uo****@naver.com 010-****-****");
        assert_eq!(mask_pii(&once), once);
    }

    #[test]
    fn 이메일이_아닌_평문_uuid_epoch는_그대로_둔다() {
        assert_eq!(
            mask_pii("GET /v1/ats/jobs/d5f67a7e-1 at 1794360739"),
            "GET /v1/ats/jobs/d5f67a7e-1 at 1794360739"
        );
    }

    // reportBucket decides whether a finding is a confirmed fact, an unproven
    // suspicion to verify, or attack-surface inventory — the split that keeps a
    // read-only scan's report from reading as 12 equal "findings".
    // The TypeScript helper's defaults are vuln / medium / candidate.

    #[test]
    fn 엔드포인트_인벤토리는_심각도와_무관하게_정찰_recon() {
        assert_eq!(
            report_bucket("endpoint", "medium", "candidate"),
            ReportBucket::Recon
        );
        assert_eq!(
            report_bucket("endpoint", "medium", "validated"),
            ReportBucket::Recon
        );
    }

    #[test]
    fn info_심각도와_accepted_risk는_정찰로_내린다_헤드라인_오염_방지() {
        assert_eq!(
            report_bucket("auth", "info", "candidate"),
            ReportBucket::Recon
        );
        assert_eq!(
            report_bucket("vuln", "medium", "accepted_risk"),
            ReportBucket::Recon
        );
    }

    #[test]
    fn validated_보안_이슈만_confirmed() {
        assert_eq!(
            report_bucket("vuln", "medium", "validated"),
            ReportBucket::Confirmed
        );
        assert_eq!(
            report_bucket("secret", "medium", "validated"),
            ReportBucket::Confirmed
        );
    }

    #[test]
    fn 실증_안_된_보안_이슈는_candidate_실제_두_건_cookie_scope_headers가_여기로() {
        assert_eq!(
            report_bucket("auth", "medium", "candidate"),
            ReportBucket::Candidate
        );
        assert_eq!(
            report_bucket("other", "medium", "candidate"),
            ReportBucket::Candidate
        );
    }

    // --- not covered by the TypeScript tests ---

    #[test]
    fn 마스킹의_경계는_자바스크립트_정규식과_같다() {
        // Each expectation is what the TypeScript maskPii returned in bun.
        let cases = [
            ("", ""),
            ("a@b.co", "a****@b.co"),
            ("x a@b.c", "x a@b.c"),
            ("ab@c.d1", "ab@c.d1"),
            ("@naver.com", "@naver.com"),
            (
                "a.b+c@sub.example.co.kr, d@e.io",
                "a.****@sub.example.co.kr, d****@e.io",
            ),
            ("１０@x.com", "１０@x.com"),
            // \b is ASCII-only in JavaScript: Hangul next to the number is a boundary.
            ("연락처010-1234-5678입니다", "연락처010-****-****입니다"),
            ("é010-1234-5678é", "é010-****-****é"),
            ("_010-1234-5678", "_010-1234-5678"),
            ("010-1234-5678_", "010-1234-5678_"),
            ("id1010-1234-5678", "id1010-1234-5678"),
            ("٠10-1234-5678", "٠10-1234-5678"),
            ("tel:0212345678", "tel:021-****-****"),
            ("031 123 4567", "031-****-****"),
            ("010\u{a0}1234\u{3000}5678", "010-****-****"),
            ("010\n1234\n5678", "010-****-****"),
            ("00-000-0000", "00-****-****"),
            ("0101234567890", "0101234567890"),
            ("010-1234-56789", "010-1234-56789"),
            ("010-123-4567 010-1234-5678", "010-****-**** 010-****-****"),
        ];
        for (input, expected) in cases {
            assert_eq!(mask_pii(input), expected, "{input:?}");
        }
        // U+FEFF is whitespace in JavaScript, U+0085 is not.
        assert_eq!(mask_pii("010\u{feff}1234-5678"), "010-****-****");
        assert_eq!(mask_pii("010\u{85}1234-5678"), "010\u{85}1234-5678");
    }

    const STORE: &str = include_str!("../fixtures/findings/store.json");

    /// A data folder holding the store the reference run of the TypeScript used.
    fn seeded() -> TempDir {
        let dir = TempDir::new();
        std::fs::write(dir.path().join(STORE_FILE), STORE).unwrap();
        dir
    }

    fn disk(dir: &TempDir) -> String {
        std::fs::read_to_string(dir.path().join(STORE_FILE)).unwrap()
    }

    fn parsed(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    const NOISE: &str = "not saved: negative / false-positive results aren't recorded (scan-noise policy) — the report carries only confirmed or candidate vulnerabilities. If you ruled OUT a vuln that was already saved, finding_remove it instead.";

    /// The same calls, in the same order, as the reference run of the TypeScript
    /// tools over `fixtures/findings/store.json`; every expected text is that
    /// run's output.
    #[tokio::test]
    async fn 타입스크립트_도구와_같은_순서로_호출하면_같은_출력을_낸다() {
        let dir = seeded();
        let page = Arc::new(Page::new(None));
        let ctx = dir.ctx_on(page.clone());

        let ok_text = |text: &str| (false, text.to_owned());
        assert_eq!(
            call(&FindingList, &ctx, json!({})).await,
            ok_text(include_str!("../fixtures/findings/list_all.json"))
        );
        assert_eq!(
            call(&FindingList, &ctx, json!({ "category": "vuln" })).await,
            ok_text(include_str!("../fixtures/findings/list_vuln.json"))
        );
        let (_, text) = call(&FindingList, &ctx, json!({ "tag": "sqli" })).await;
        let ids = |text: &str| -> Vec<String> {
            let list = parsed(text);
            let ids = list.as_array().unwrap().iter();
            ids.map(|f| f["id"].as_str().unwrap().to_owned()).collect()
        };
        assert_eq!(ids(&text), ["v1"]);
        let (_, text) = call(&FindingList, &ctx, json!({ "severity": "medium" })).await;
        assert_eq!(ids(&text), ["o1", "r1", "v2"]);
        // Reading does not rewrite the store.
        assert_eq!(disk(&dir), STORE);

        assert_eq!(
            call(&FindingExport, &ctx, json!({})).await,
            (
                true,
                "no scope: pass site with the scanned URL (e.g. site: \"http://localhost:1337\") or all=true — otherwise I cannot tell which site the report covers.".to_owned()
            )
        );
        assert_eq!(
            call(
                &FindingExport,
                &ctx,
                json!({ "site": "https://www.shop.test/cart" })
            )
            .await,
            ok_text(include_str!("../fixtures/findings/export_site.md"))
        );
        page.go(Some("http://127.0.0.1:1337/x"));
        assert_eq!(
            call(&FindingExport, &ctx, json!({})).await,
            ok_text(include_str!("../fixtures/findings/export_tab.md"))
        );
        page.go(None);
        assert_eq!(
            call(&FindingExport, &ctx, json!({ "all": true })).await,
            ok_text(include_str!("../fixtures/findings/export_all.md"))
        );
        assert_eq!(
            call(
                &FindingExport,
                &ctx,
                json!({ "site": "https://nothing.test/" })
            )
            .await,
            ok_text("No findings recorded yet for nothing.test.")
        );
        assert_eq!(
            call(&FindingRemove, &ctx, json!({ "id": "nope" })).await,
            (true, "unknown finding: nope".to_owned())
        );

        let dupe = json!({
            "title": "items again", "body": "see https://API.shop.test//v1/items?page=2",
            "category": "endpoint"
        });
        assert_eq!(
            call(&FindingAdd, &ctx, dupe).await,
            ok_text(
                "not saved: an endpoint finding for the same host+path already exists — #e2 \"shop.test item list\" (low, candidate).\n\
                 If this adds new evidence, record it on that finding instead (its body/evidence), or finding_remove it first.\n\
                 If this is a genuinely different endpoint, include the full URL in the title."
            )
        );
        let noise = json!({ "title": "t", "body": "b", "tags": ["NEGATIVE"] });
        assert_eq!(call(&FindingAdd, &ctx, noise).await, ok_text(NOISE));
        let false_positive = json!({ "title": "t", "body": "b", "status": "false_positive" });
        assert_eq!(
            call(&FindingAdd, &ctx, false_positive).await,
            ok_text(NOISE)
        );
        // Nothing was saved so far, so nothing was written.
        assert_eq!(disk(&dir), STORE);

        // The reply carries a fresh UUID; everything else is fixed.
        let reply = |text: &str| -> (String, Value) {
            let mut reply = parsed(text);
            let id = reply["id"].as_str().unwrap().to_owned();
            assert!(Uuid::parse_str(&id).is_ok(), "{id}");
            assert_eq!(pretty(&reply), text);
            reply["id"] = json!("<id>");
            (pretty(&reply), reply)
        };

        page.go(Some("http://127.0.0.1:1337/x"));
        let downgrade = json!({
            "title": "Mail leak bob@corp.test", "body": "call 02-123-4567",
            "status": "validated", "requestId": "", "evidenceIds": ["a"],
            "severity": "high", "category": "vuln",
            "reproCommand": "curl -u alice@corp.test x", "tags": ["pii"]
        });
        let (is_error, text) = call(&FindingAdd, &ctx, downgrade).await;
        assert!(!is_error);
        assert_eq!(
            reply(&text).0,
            r#"{
  "id": "<id>",
  "title": "Mail leak bob@corp.test",
  "category": "vuln",
  "severity": "high",
  "status": "candidate",
  "evidenceCount": 1,
  "warning": "status downgraded to 'candidate': 'validated' needs ≥2 independent evidence items, got 1. Add more evidenceIds (best: 1 static + 1 dynamic) or record it as residual risk."
}"#
        );
        page.go(None);
        let (_, text) = call(
            &FindingAdd,
            &ctx,
            json!({ "title": "plain", "body": "note" }),
        )
        .await;
        assert_eq!(
            reply(&text).0,
            r#"{
  "id": "<id>",
  "title": "plain",
  "category": "other",
  "severity": "info",
  "status": "candidate",
  "evidenceCount": 0
}"#
        );
        let valid = json!({
            "title": "two", "body": "ev", "status": "validated",
            "requestId": "r1", "evidenceIds": ["a"]
        });
        let (_, text) = call(&FindingAdd, &ctx, valid).await;
        assert_eq!(
            reply(&text).0,
            r#"{
  "id": "<id>",
  "title": "two",
  "category": "other",
  "severity": "info",
  "status": "validated",
  "evidenceCount": 2
}"#
        );
        assert_eq!(
            call(&FindingRemove, &ctx, json!({ "id": "e1" })).await,
            ok_text("removed e1")
        );

        // The store on disk: the legacy entries healed in place, the three new
        // ones appended with PII masked. Their ids and timestamps are this run's.
        let mut stored = parsed(&disk(&dir));
        let entries = stored.as_array_mut().unwrap();
        let fresh = entries.len() - 3;
        for entry in &mut entries[fresh..] {
            assert!(Uuid::parse_str(entry["id"].as_str().unwrap()).is_ok());
            assert!(entry["createdAt"].as_u64().unwrap() > 1_700_000_000_000);
            entry["id"] = json!("<id>");
            entry["createdAt"] = json!(0);
        }
        assert_eq!(
            pretty(&stored),
            include_str!("../fixtures/findings/disk_after.json")
        );

        assert_eq!(
            clear_findings_for_site(&ctx, "http://localhost:1337/abc"),
            3
        );
        assert_eq!(clear_findings_for_site(&ctx, "not a url"), 0);
        assert_eq!(
            clear_findings_for_site(&ctx, "http://localhost:1337/abc"),
            0
        );
        let left = parsed(&disk(&dir));
        let origins = left.as_array().unwrap().iter();
        assert!(origins.clone().all(|f| f["origin"] != "localhost:1337"));
        // 12 seeded + 3 added - e1 removed - 3 cleared.
        assert_eq!(origins.count(), 11);
    }

    #[tokio::test]
    async fn 목록_필터는_함께_걸리고_빈_태그는_거르지_않는다() {
        // Each expectation is what the TypeScript tool listed for the same store.
        let dir = seeded();
        let ctx = dir.ctx(None);
        let mut listed = Vec::new();
        for args in [
            json!({ "tag": "" }),
            json!({ "category": "vuln", "severity": "high" }),
            json!({ "tag": "inventory", "category": "endpoint" }),
            json!({ "tag": "inventory", "category": "vuln" }),
            // Tags are matched exactly, case included.
            json!({ "tag": "Inventory" }),
        ] {
            let (is_error, text) = call(&FindingList, &ctx, args).await;
            assert!(!is_error);
            let list = parsed(&text);
            let ids = list.as_array().unwrap().iter();
            listed.push(
                ids.map(|f| f["id"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>(),
            );
        }
        let all = [
            "l2", "o1", "s1", "r1", "n2", "n1", "v3", "v2", "v1", "e2", "e1", "l1",
        ];
        assert_eq!(listed[0], all);
        assert_eq!(listed[1], ["v1"]);
        assert_eq!(listed[2], ["e2"]);
        assert!(listed[3].is_empty());
        assert!(listed[4].is_empty());
    }

    #[tokio::test]
    async fn 저장한_발견은_새_컨텍스트에서도_다시_읽힌다() {
        let dir = TempDir::new();
        let ctx = dir.ctx(Some("https://app.example.com/login"));
        let args = json!({ "title": "Weak cookie", "body": "No HttpOnly", "category": "auth" });
        let (is_error, text) = call(&FindingAdd, &ctx, args).await;
        assert!(!is_error);
        let id = parsed(&text)["id"].as_str().unwrap().to_owned();

        let later = dir.ctx(None);
        let list = list_findings(&later);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].text("id"), id);
        assert_eq!(list[0].origin(), Some("example.com"));
        let keys: Vec<&String> = list[0].0.keys().collect();
        assert_eq!(
            keys,
            [
                "id",
                "title",
                "severity",
                "category",
                "body",
                "evidenceIds",
                "status",
                "tags",
                "origin",
                "createdAt"
            ]
        );
    }

    #[tokio::test]
    async fn 데이터_폴더가_없어도_저장할_때_만든다() {
        let dir = TempDir::new();
        let nested = dir.path().join("not").join("yet");
        let ctx = ToolContext::new(Arc::default(), Arc::new(Page::new(None)), nested.clone());
        assert!(list_findings(&ctx).is_empty());
        let (is_error, _) = call(&FindingAdd, &ctx, json!({ "title": "t", "body": "b" })).await;
        assert!(!is_error);
        assert!(nested.join("findings.json").exists());
    }

    #[tokio::test]
    async fn 같은_밀리초의_발견은_저장_순서를_유지하고_최신이_먼저다() {
        let dir = TempDir::new();
        let store = json!([
            { "id": "a", "title": "a", "createdAt": 5 },
            { "id": "b", "title": "b", "createdAt": 9 },
            { "id": "c", "title": "c", "createdAt": 5 },
            { "id": "d", "title": "d" }
        ]);
        std::fs::write(dir.path().join(STORE_FILE), store.to_string()).unwrap();
        let ctx = dir.ctx(None);
        let ids: Vec<String> = list_findings(&ctx)
            .iter()
            .map(|f| f.text("id").to_owned())
            .collect();
        assert_eq!(ids, ["b", "a", "c", "d"]);
    }

    #[tokio::test]
    async fn 깨진_저장_파일은_빈_저장소로_읽고_패닉하지_않는다() {
        for broken in ["{ not json", "{\"id\":\"x\"}", "\"text\"", "", "\u{feff}[]"] {
            let dir = TempDir::new();
            std::fs::write(dir.path().join(STORE_FILE), broken).unwrap();
            let ctx = dir.ctx(None);
            assert_eq!(
                call(&FindingList, &ctx, json!({})).await,
                (false, "[]".to_owned()),
                "{broken:?}"
            );
            // Loading alone never rewrites the file.
            assert_eq!(disk(&dir), broken);
        }
        let dir = TempDir::new();
        std::fs::write(dir.path().join(STORE_FILE), [b'[', 0xff, b']']).unwrap();
        assert!(list_findings(&dir.ctx(None)).is_empty());
    }

    #[tokio::test]
    async fn 반쪽_서로게이트_이스케이프가_있는_저장소도_읽고_덮어쓰지_않는다() {
        // What JSON.stringify writes for a string holding half a surrogate pair;
        // JSON.parse (the TypeScript) reads it back.
        let store = r#"[{"id":"a","title":"lone \ud800 x","severity":"low","body":"pair \ud83d\ude00 low \udc00 kept \\ud800 end \ud83d","createdAt":1}]"#;
        let dir = TempDir::new();
        std::fs::write(dir.path().join(STORE_FILE), store).unwrap();
        let ctx = dir.ctx(None);
        let list = list_findings(&ctx);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].text("title"), "lone \u{fffd} x");
        assert_eq!(
            list[0].text("body"),
            "pair 😀 low \u{fffd} kept \\ud800 end \u{fffd}"
        );
        // The next save keeps the earlier finding instead of replacing the file.
        let (is_error, _) = call(&FindingAdd, &ctx, json!({ "title": "t", "body": "b" })).await;
        assert!(!is_error);
        let stored = parsed(&disk(&dir));
        let titles: Vec<&Value> = stored
            .as_array()
            .unwrap()
            .iter()
            .map(|f| &f["title"])
            .collect();
        assert_eq!(titles, [&json!("lone \u{fffd} x"), &json!("t")]);

        // Only the halves change: pairs, other escapes and broken tails pass through.
        assert_eq!(
            without_lone_surrogates(r#""\uD83D\uDE00\uDBFF\n\u00e9""#),
            r#""\uD83D\uDE00\ufffd\n\u00e9""#
        );
        for untouched in [r#"["\u12"]"#, "[\"\\", r#""\\\uD83D\uDE00é\"#, ""] {
            assert_eq!(without_lone_surrogates(untouched), untouched);
        }
        // A store that is broken in another way is still read as empty.
        let truncated = TempDir::new();
        std::fs::write(
            truncated.path().join(STORE_FILE),
            r#"[{"id":"a","title":"\ud800"#,
        )
        .unwrap();
        assert!(list_findings(&truncated.ctx(None)).is_empty());
    }

    #[tokio::test]
    async fn 모양이_이상한_항목도_읽고_보고서를_만든다() {
        let dir = TempDir::new();
        let store = json!([
            null,
            7,
            "text",
            { "id": "x", "title": 5, "severity": "urgent", "category": "misc", "body": null,
              "tags": "nope", "evidenceIds": [1, "ev"], "status": 3, "origin": 9,
              "createdAt": "soon" },
            { "id": "y", "title": "first", "createdAt": 1 },
            { "id": "y", "title": "second", "severity": "high", "category": "vuln", "createdAt": 2 }
        ]);
        std::fs::write(dir.path().join(STORE_FILE), store.to_string()).unwrap();
        let ctx = dir.ctx(None);
        let list = list_findings(&ctx);
        // The repeated id keeps one entry, the later one.
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].text("title"), "second");
        // A non-string origin is dropped; the other odd values are kept as they are.
        assert_eq!(list[1].0.get("origin"), None);
        assert_eq!(list[1].0["tags"], "nope");

        let (is_error, text) = call(&FindingExport, &ctx, json!({ "all": true })).await;
        assert!(!is_error);
        assert_eq!(
            text,
            "# Reversing Session Findings\n\n\
             **0 confirmed · 2 to verify · 0 recon**\n\n\
             ## Needs verification (2)\n\n\
             _Unproven — this read-only scan could not confirm these. Check the code or a live request before acting; absence of a client-side control is not proof of a server-side one._\n\n\
             ### second\n_🟠 high · Vulnerabilities · status: candidate_\n\n\n\n\
             ### \n_ ·  · status: _\n\n\n\n\
             **Evidence:** `ev`\n"
        );
        let (is_error, _) = call(&FindingList, &ctx, json!({ "tag": "nope" })).await;
        assert!(!is_error);
    }

    #[tokio::test]
    async fn validated는_증거가_둘_이상일_때만_유지된다() {
        let dir = TempDir::new();
        let ctx = dir.ctx(None);
        let add = |extra: Value| {
            let mut args = json!({ "title": "claim", "body": "b", "status": "validated" });
            for (key, value) in extra.as_object().cloned().unwrap_or_default() {
                args[key] = value;
            }
            args
        };
        let status = |text: &str| {
            let reply = parsed(text);
            (
                reply["status"].as_str().unwrap().to_owned(),
                reply["evidenceCount"].as_u64().unwrap(),
                reply.get("warning").is_some(),
            )
        };
        let (_, text) = call(&FindingAdd, &ctx, add(json!({}))).await;
        assert_eq!(status(&text), ("candidate".to_owned(), 0, true));
        let (_, text) = call(&FindingAdd, &ctx, add(json!({ "requestId": "r" }))).await;
        assert_eq!(status(&text), ("candidate".to_owned(), 1, true));
        let (_, text) = call(&FindingAdd, &ctx, add(json!({ "evidenceIds": ["a", "b"] }))).await;
        assert_eq!(status(&text), ("validated".to_owned(), 2, false));
        // What is stored is the downgraded status, not the requested one.
        let stored: Vec<String> = list_findings(&ctx)
            .iter()
            .map(|f| f.text("status").to_owned())
            .collect();
        assert_eq!(stored.iter().filter(|s| *s == "validated").count(), 1);
        assert_eq!(stored.len(), 3);
    }

    #[tokio::test]
    async fn url을_넘기면_파인딩에_그대로_저장된다() {
        let dir = TempDir::new();
        let ctx = dir.ctx(None);
        let (_, text) = call(
            &FindingAdd,
            &ctx,
            json!({
                "title": "Admin panel exposed",
                "body": "GET /admin/secret.html",
                "url": "http://127.0.0.1:8777/admin/secret.html"
            }),
        )
        .await;
        assert!(!text.starts_with("not saved"), "{text}");
        let stored = list_findings(&ctx);
        assert_eq!(
            stored[0].text("url"),
            "http://127.0.0.1:8777/admin/secret.html"
        );
    }

    #[tokio::test]
    async fn 잘못된_인자는_저장소를_건드리기_전에_거부한다() {
        let dir = TempDir::new();
        let ctx = dir.ctx(None);
        let (is_error, text) = call(&FindingAdd, &ctx, json!({ "title": "t" })).await;
        assert!(is_error);
        assert!(text.starts_with(
            "MCP error -32602: Input validation error: Invalid arguments for tool finding_add: "
        ));
        assert!(text.contains("Invalid input: expected string, received undefined"));
        let bad = json!({ "title": "t", "body": "b", "severity": "urgent", "tags": [1] });
        let (is_error, text) = call(&FindingAdd, &ctx, bad).await;
        assert!(is_error);
        assert!(text.contains(
            r#"Invalid option: expected one of \"info\"|\"low\"|\"medium\"|\"high\"|\"critical\""#
        ));
        assert!(text.contains("Invalid input: expected string, received number"));
        for (tool, args) in [
            (&FindingList as &dyn Tool, json!({ "category": "misc" })),
            (&FindingExport, json!({ "all": "yes" })),
            (&FindingRemove, json!({})),
            (&FindingRemove, Value::Null),
        ] {
            assert!(tool.call(&ctx, args).await.is_error);
        }
        assert!(!dir.path().join(STORE_FILE).exists());
    }

    #[tokio::test]
    async fn 내보내기는_site가_탭보다_우선이고_all은_사이트_표시를_뺀다() {
        let dir = seeded();
        let ctx = dir.ctx(Some("http://127.0.0.1:1337/x"));
        let site = json!({ "site": "https://www.shop.test/cart" });
        let (_, text) = call(&FindingExport, &ctx, site).await;
        assert!(text.starts_with("# Reversing Session Findings\n\n_site: shop.test_\n\n"));
        // A site that is not a URL falls back to the tab.
        let (_, text) = call(&FindingExport, &ctx, json!({ "site": "shop.test" })).await;
        assert!(text.starts_with("# Reversing Session Findings\n\n_site: localhost:1337_\n\n"));
        let all = json!({ "site": "https://www.shop.test/cart", "all": true });
        let (_, text) = call(&FindingExport, &ctx, all).await;
        assert!(text.starts_with("# Reversing Session Findings\n\n**2 confirmed"));

        let empty = TempDir::new();
        assert_eq!(
            call(&FindingExport, &empty.ctx(None), json!({ "all": true })).await,
            (false, "No findings recorded yet.".to_owned())
        );
    }

    #[tokio::test]
    async fn 동시에_들어온_같은_엔드포인트는_하나만_저장한다() {
        let dir = TempDir::new();
        let ctx = Arc::new(dir.ctx(Some("https://shop.test/")));
        let workers: Vec<_> = (0..8)
            .map(|n| {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let args = json!({
                        "title": format!("tab {n}: GET https://api.shop.test/v1/items"),
                        "body": "public", "category": "endpoint"
                    });
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .build()
                        .unwrap();
                    runtime.block_on(FindingAdd.call(&ctx, args))
                })
            })
            .collect();
        for worker in workers {
            assert!(!worker.join().unwrap().is_error);
        }
        assert_eq!(list_findings(&ctx).len(), 1);
    }

    #[test]
    fn 목록에_없는_심각도와_분류는_상세에서는_비우고_정찰_줄에서는_undefined로_쓴다() {
        // What the TypeScript rendered for this store: Array.join drops the
        // undefined label, the template literal of the recon line spells it out.
        let odd = |id: &str, status: &str, created_at: u64| {
            Finding(
                json!({
                    "id": id, "title": id, "body": "b", "severity": "urgent",
                    "category": "misc", "status": status, "tags": [], "evidenceIds": [],
                    "createdAt": created_at
                })
                .as_object()
                .cloned()
                .unwrap(),
            )
        };
        let report = render_markdown(
            &[odd("odd", "accepted_risk", 1), odd("odd2", "candidate", 2)],
            None,
            &RobotsNotes::default(),
        );
        assert!(
            report.ends_with(
                "### odd2\n_ ·  · status: candidate_\n\nb\n\n\
                 ## Attack surface & recon (1)\n\n\
                 - **odd** _(undefined, undefined)_\n"
            ),
            "{report}"
        );
    }

    #[test]
    fn 보고서의_중복_병합_문구는_단수와_복수를_구분한다() {
        let endpoint = |id: &str, created_at: u64| {
            Finding(
                json!({
                    "id": id, "title": "GET https://a.test/x", "severity": "info",
                    "category": "endpoint", "body": "b", "evidenceIds": [],
                    "status": "candidate", "tags": [], "createdAt": created_at
                })
                .as_object()
                .cloned()
                .unwrap(),
            )
        };
        let two = [endpoint("a", 1), endpoint("b", 2)];
        assert!(
            render_markdown(&two, None, &RobotsNotes::default()).contains(
                "**0 confirmed · 0 to verify · 1 recon** _(1 duplicate report merged)_\n"
            )
        );
        let three = [endpoint("a", 1), endpoint("b", 2), endpoint("c", 3)];
        assert!(render_markdown(&three, Some("a.test"), &RobotsNotes::default()).contains(
            "_site: a.test_\n\n**0 confirmed · 0 to verify · 1 recon** _(2 duplicate reports merged)_\n"
        ));
    }

    #[test]
    fn robots_차단_경로_파인딩에는_비고가_붙는다() {
        let ctx = ToolContext::for_tests();
        let patch = rever_core::traffic::RequestPatch {
            request_id: "r1".to_owned(),
            url: Some("http://a.test/robots.txt".to_owned()),
            method: Some("GET".to_owned()),
            status: Some(200),
            response_body: Some("User-agent: *\nDisallow: /admin\n".to_owned()),
            started_at: Some(1),
            ..Default::default()
        };
        ctx.traffic
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .upsert(patch, 1);
        let notes = RobotsNotes::from_traffic(&ctx);

        let finding = |url: &str| {
            Finding(
                json!({
                    "id": url, "title": format!("GET {url}"), "severity": "info",
                    "category": "endpoint", "body": "b", "evidenceIds": [],
                    "status": "candidate", "tags": [], "createdAt": 1, "url": url,
                    "origin": "a.test"
                })
                .as_object()
                .cloned()
                .unwrap(),
            )
        };
        let report = render_markdown(
            &[
                finding("http://a.test/admin/panel"),
                finding("http://a.test/public"),
            ],
            Some("a.test"),
            &notes,
        );
        assert!(
            report.contains("robots-disallowed"),
            "disallowed recon line carries the mark: {report}"
        );
        assert!(
            report.contains("GET http://a.test/public"),
            "allowed path stays unmarked: {report}"
        );
    }

    #[test]
    fn 캡처_없으면_robots_비고도_없다() {
        // No captured robots.txt means no claim, even for /admin paths.
        let finding = Finding(
            json!({
                "id": "f", "title": "Admin panel exposed", "severity": "high",
                "category": "access", "body": "b", "evidenceIds": [],
                "status": "candidate", "tags": [], "createdAt": 1,
                "url": "http://a.test/admin", "origin": "a.test"
            })
            .as_object()
            .cloned()
            .unwrap(),
        );
        let report = render_markdown(&[finding], None, &RobotsNotes::default());
        assert!(
            !report.contains("robots: disallowed path"),
            "no capture means no claim: {report}"
        );
    }
}
