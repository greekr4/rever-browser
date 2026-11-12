//! Port of `src/main/mcp/findings-dedupe.ts` — pure finding-dedup helpers.
//!
//! Twenty parallel scan workers each record the same public endpoint; without
//! a key the export report repeats one endpoint once per tab that saw it.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::js_regex;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupableFinding {
    pub id: String,
    pub title: String,
    pub body: String,
    pub category: String,
    pub severity: String,
    pub status: String,
    pub tags: Vec<String>,
    pub evidence_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Epoch milliseconds.
    pub created_at: i64,
    /// Every other field of the stored finding. The TypeScript is generic over
    /// `T extends DedupableFinding` and a merge keeps the primary's fields with
    /// `...primary`; carrying them here does the same without a trait.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeResult {
    pub merged: Vec<DedupableFinding>,
    pub merged_away: usize,
}

fn severity_rank(severity: &str) -> Option<u8> {
    Some(match severity {
        "critical" => 5,
        "high" => 4,
        "medium" => 3,
        "low" => 2,
        "info" => 1,
        _ => return None,
    })
}

/// A pure-negative result ("I checked X and it's fine / not a vuln") is scan
/// noise, not a deliverable — a weak worker floods the report with "static CSS
/// served without auth = expected" notes instead of the one real vuln. Workers
/// label these themselves: `false_positive` status, or a `negative` tag. Enforce
/// deterministically so prose ("don't record negatives") a model can ignore
/// becomes a rule it can't. The report carries only confirmed/candidate vulns.
pub fn is_scan_noise<S: AsRef<str>>(status: &str, tags: &[S]) -> bool {
    status == "false_positive" || tags.iter().any(|t| t.as_ref().to_lowercase() == "negative")
}

/// Canonicalise a path for keying: collapse duplicate slashes and strip the
/// trailing one. The `//get_ticket` vs `/get_ticket` of an ACL-bypass write-up is
/// the SAME endpoint for dedup (the raw form stays in the body); without this the
/// bypass's own double-slash scatters the key across restatements.
fn canon_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c != '/' || !out.ends_with('/') {
            out.push(c);
        }
    }
    out.truncate(out.trim_end_matches('/').len());
    if out.is_empty() {
        out.push('/');
    }
    out
}

static URL_RE: LazyLock<Regex> = LazyLock::new(|| js_regex(r#"(?i-u:https?)://[^\s`)\]"'<>]+"#));
static BARE_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r#"(?:^|[\s(`"'])(/[A-Za-z0-9\-_./]{2,})"#));
static DOMAIN_RE: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"\b([A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+)\b"));

/// host(:port)+path of the first URL in the text, or first bare-path form.
fn host_path_of(text: &str) -> Option<String> {
    if let Some(found) = URL_RE.find(text) {
        let u = Url::parse(found.as_str()).ok()?;
        let host = u.host_str().unwrap_or("").to_lowercase();
        let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
        // The url crate leaves '^' raw in a path; `new URL()` percent-encodes it.
        let path = u.path().replace('^', "%5E");
        return Some(format!("{host}{port}{}", canon_path(&path)));
    }
    // No absolute URL: workers write titles like "site.com — public API (/api/x)".
    // Combine the first domain-like token with the first bare path so that form
    // still lands on the same key as the full-URL form.
    let path = canon_path(BARE_PATH_RE.captures(text)?.get(1)?.as_str());
    Some(match DOMAIN_RE.captures(text).and_then(|c| c.get(1)) {
        Some(domain) => format!("{}{path}", domain.as_str().to_lowercase()),
        None => path,
    })
}

/// Identity of an endpoint finding: host + path of the first URL in the
/// title/body (query stripped), or the first bare absolute path when no URL
/// appears. None = no key (never deduped) — only 'endpoint' inventory entries
/// are keyed here; vuln findings get their own class-scoped key below.
pub fn endpoint_key_of(f: &DedupableFinding) -> Option<String> {
    if f.category != "endpoint" {
        return None;
    }
    host_path_of(&format!("{}\n{}", f.title, f.body))
}

// Class of a vuln claim, matched on title + tags. Order matters: specific
// classes before generic ones ("Header Injection" is crlf, not headers).
//
// The TypeScript patterns carry /i; here the text is ASCII-lowercased first and
// the patterns are case-sensitive, which is the same folding JavaScript applies
// without the u flag (no "K" KELVIN SIGN matching "k").
static VULN_CLASS_PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        ("cmdi", r"command.?inject|\bcmdi\b|\brce\b"),
        ("ssti", r"template.?inject|\bssti\b"),
        ("sqli", r"sql.?inject|\bsqli\b"),
        ("ssrf", r"\bssrf\b|server.?side.?request"),
        ("lfi", r"\blfi\b|path.?traversal|local.?file.?incl"),
        ("idor", r"\bidor\b|\bbola\b|broken.?object"),
        ("crlf", r"\bcrlf\b|header.?inject"),
        ("redirect", r"open.?redirect"),
        ("pathwrite", r"arbitrary.?file.?write|path.?travers|file.?write"),
        (
            "headers",
            r"security[\s\w-]{0,20}?headers?\b|missing[^\n]*headers?\b|response[\s-]?header|header[\s-]?posture|\bcsp\b|content.?security|hsts|x.?frame|nosniff|clickjack",
        ),
        ("cors", r"\bcors\b|wildcard.?origin|access.?control.?allow.?origin|\bacao\b"),
        (
            "csrf",
            r"\bcsrf\b|cross.?site.?request.?forgery|state.?chang|content.?injection|content.?spoof",
        ),
        ("xss", r"\bxss\b|cross.?site.?script"),
        (
            "accesscontrol",
            r"\bacl\b|access.?control|path.?normaliz|path.?confus|proxy.?(deny|bypass)|haproxy|deny.?rule",
        ),
        ("jwt", r"\bjwt\b|token.?forge|\bjws\b|alg.?confusion|python_jwt|jwcrypto"),
        ("auth", r"broken.?auth|auth.?bypass|unauth"),
        ("secret", r"\bsecret\b|api.?key|credential|token.?leak"),
    ]
    .into_iter()
    // JavaScript's `.` also refuses \r, U+2028 and U+2029, not only \n, and it
    // matches one UTF-16 unit — so a lone `.?` never spans an astral character
    // (an emoji between two words is two units there).
    .map(|(class, pattern)| {
        let any = r"[^\n\r\u{2028}\u{2029}\u{10000}-\u{10FFFF}]?";
        (class, js_regex(&pattern.replace(".?", any)))
    })
    .collect()
});

fn vuln_class_of(f: &DedupableFinding) -> Option<&'static str> {
    let text = format!("{} {}", f.title, f.tags.join(" ")).to_ascii_lowercase();
    VULN_CLASS_PATTERNS
        .iter()
        .find(|(_, re)| re.is_match(&text))
        .map(|(class, _)| *class)
}

static VULN_PARAM_PATTERNS: LazyLock<[Regex; 3]> = LazyLock::new(|| {
    [
        js_regex(r#"[`'"](\w{2,})[`'"]\s*\b(?i-u:param(?:eter)?)\b"#),
        js_regex(r"[?&](\w+)="),
        js_regex(r#"\b(?i-u:param(?:eter)?)\s*[:=]\s*"?(\w+)"?"#),
    ]
});

// The injection site: backticked word before "param(parameter)", ?param= in the
// title, or param:"x" style. '' when not stated — same class+endpoint with no
// param stated merges (the claim is the same sink).
fn vuln_param_of(title: &str) -> &str {
    VULN_PARAM_PATTERNS
        .iter()
        .find_map(|re| re.captures(title).and_then(|c| c.get(1)))
        .map_or("", |m| m.as_str())
}

const HOST_WIDE_CLASSES: [&str; 2] = ["headers", "cors"];

/// Identity of a vuln finding: vulnerability class + endpoint + param. N probe
/// workers each validate the same sink (e.g. reflected XSS on ?format=) and
/// report it with different titles — class|endpoint|param is the shared claim.
/// Covers 'vuln' and 'other' (misconfig/headers/CSRF land in 'other' and dupe
/// just as often). None = never deduped: unknown class, or auth/secret/endpoint
/// categories whose same-endpoint findings stay separate claims.
pub fn vuln_key_of(f: &DedupableFinding) -> Option<String> {
    if f.category != "vuln" && f.category != "other" {
        return None;
    }
    let class = vuln_class_of(f)?;
    let ep = host_path_of(&format!("{}\n{}", f.title, f.body))?;
    // Host-wide issues (no security headers, wildcard CORS) are a property of the
    // server, not one route — workers report them once per endpoint they touch.
    // Key by host alone so N endpoints collapse to one (merge keeps max severity,
    // so a real CORS-on-login medium isn't lost in an asset-CORS low).
    if HOST_WIDE_CLASSES.contains(&class) {
        let host = ep
            .split('/')
            .next()
            .filter(|h| !h.is_empty())
            .unwrap_or(&ep);
        return Some(format!("vulnlike|{class}|{host}"));
    }
    Some(format!("vulnlike|{class}|{ep}|{}", vuln_param_of(&f.title)))
}

fn dedup_key_of(f: &DedupableFinding) -> Option<String> {
    endpoint_key_of(f).or_else(|| vuln_key_of(f))
}

/// An empty request id counts as absent, as it does in the TypeScript truthiness check.
fn request_id_of(f: &DedupableFinding) -> Option<&String> {
    f.request_id.as_ref().filter(|id| !id.is_empty())
}

/// Insertion-ordered union, like spreading into a `Set`.
fn union<'a>(items: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

// JavaScript's `.length`: bodies mix Hangul and ASCII, and comparing UTF-8 byte
// lengths would pick a different primary than the Electron app did.
fn js_length(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Merge findings that share a key (endpoint inventory: host+path; vuln claims:
/// class+endpoint+param): the longest body becomes the primary, evidence/tags
/// union, highest severity wins, earliest createdAt keeps. Everything else
/// passes through untouched. Order is preserved (first occurrence position).
fn absorb_finding(prev: DedupableFinding, f: DedupableFinding) -> DedupableFinding {
    let (mut primary, absorbed) = if js_length(&f.body) > js_length(&prev.body) {
        (f, prev)
    } else {
        (prev, f)
    };
    // An unknown severity on either side falls to the absorbed one, as the
    // `undefined >= n` comparison does.
    let primary_outranks = matches!(
        (severity_rank(&primary.severity), severity_rank(&absorbed.severity)),
        (Some(p), Some(a)) if p >= a
    );
    let tags = union(primary.tags.iter().chain(&absorbed.tags));
    let mut evidence_ids = union(
        primary
            .evidence_ids
            .iter()
            .chain(&absorbed.evidence_ids)
            .chain(request_id_of(&primary))
            .chain(request_id_of(&absorbed)),
    );
    evidence_ids.truncate(12);

    primary.tags = tags;
    primary.evidence_ids = evidence_ids;
    primary.created_at = primary.created_at.min(absorbed.created_at);
    if absorbed.status == "validated" {
        primary.status = absorbed.status;
    }
    if !primary_outranks {
        primary.severity = absorbed.severity;
    }
    primary
}

fn evidence_ids_of(f: &DedupableFinding) -> impl Iterator<Item = &String> {
    request_id_of(f).into_iter().chain(&f.evidence_ids)
}

// Two findings are DIFFERENT claims only when both carry a known, distinct vuln
// class — then a shared evidence id (a request both cite) must not merge them
// (e.g. an ACL bypass and a JWT forgery that both reference /get_ticket).
fn class_conflict(a: &DedupableFinding, b: &DedupableFinding) -> bool {
    matches!((vuln_class_of(a), vuln_class_of(b)), (Some(ca), Some(cb)) if ca != cb)
}

pub fn merge_duplicate_endpoints(list: Vec<DedupableFinding>) -> MergeResult {
    let total = list.len();
    let mut reps: Vec<DedupableFinding> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut by_evidence: HashMap<String, usize> = HashMap::new(); // requestId/evidenceId → rep index

    for f in list {
        let mut found = dedup_key_of(&f).and_then(|key| by_key.get(&key).copied());
        // Fallback: a vuln-like finding sharing a request/evidence id with an earlier
        // one is the same claim restated — unless they are distinctly classed.
        if found.is_none() && f.category != "endpoint" {
            found = evidence_ids_of(&f).find_map(|e| {
                by_evidence
                    .get(e)
                    .copied()
                    .filter(|&cand| reps.get(cand).is_some_and(|rep| !class_conflict(rep, &f)))
            });
        }
        let idx = match found.and_then(|i| reps.get_mut(i).map(|slot| (i, slot))) {
            Some((i, slot)) => {
                *slot = absorb_finding(std::mem::take(slot), f);
                i
            }
            None => {
                reps.push(f);
                reps.len() - 1
            }
        };
        let Some(rep) = reps.get(idx) else { continue };
        if let Some(key) = dedup_key_of(rep) {
            by_key.insert(key, idx);
        }
        // Evidence index is the fallback signal for vuln-like findings only; endpoint
        // inventory already dedups by host+path and must not absorb a vuln.
        if rep.category != "endpoint" {
            for e in evidence_ids_of(rep) {
                by_evidence.entry(e.clone()).or_insert(idx);
            }
        }
    }
    MergeResult {
        merged_away: total.saturating_sub(reps.len()),
        merged: reps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TypeScript helper's defaults; tests override fields with struct update syntax.
    fn f(title: &str, body: &str) -> DedupableFinding {
        DedupableFinding {
            id: "t".into(),
            title: title.into(),
            body: body.into(),
            category: "endpoint".into(),
            severity: "low".into(),
            status: "candidate".into(),
            created_at: 1_700_000_000_000,
            ..DedupableFinding::default()
        }
    }

    fn vuln(title: &str, body: &str) -> DedupableFinding {
        DedupableFinding {
            category: "vuln".into(),
            ..f(title, body)
        }
    }

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn sorted(items: &[String]) -> Vec<String> {
        let mut out = items.to_vec();
        out.sort();
        out
    }

    // ── endpointKeyOf ───────────────────────────────────────────────────────

    #[test]
    fn 같은_host와_path는_같은_키를_낸다() {
        let a = endpoint_key_of(&f("댓글 API", "GET https://site.com/api/comments?slug=x"));
        let b = endpoint_key_of(&f("site.com — comments API (/api/comments)", "다른 관측"));
        assert_eq!(a.as_deref(), Some("site.com/api/comments"));
        assert_eq!(b.as_deref(), Some("site.com/api/comments"));
    }

    #[test]
    fn url이_없으면_첫_절대경로를_키로_쓴다() {
        let k = endpoint_key_of(&f("calc — 비인증 API (/api/comments)", "관측만"));
        assert_eq!(k.as_deref(), Some("/api/comments"));
    }

    #[test]
    fn 같은_host라도_포트가_다르면_다른_사이트로_본다() {
        let a = endpoint_key_of(&f("A", "http://localhost:1337/api/x"));
        let b = endpoint_key_of(&f("B", "http://localhost:8080/api/x"));
        assert_eq!(a.as_deref(), Some("localhost:1337/api/x"));
        assert_eq!(b.as_deref(), Some("localhost:8080/api/x"));
    }

    #[test]
    fn endpoint_카테고리가_아니면_키가_없다() {
        let k = endpoint_key_of(&vuln("SQLi in /api/comments", "https://x.com/api/comments"));
        assert_eq!(k, None);
    }

    // ── vulnKeyOf — host-wide issues ignore the path ────────────────────────

    #[test]
    fn 보안_헤더_부재는_엔드포인트가_달라도_호스트로_한_키에_모인다() {
        let a = vuln_key_of(&vuln(
            "Missing security headers (CSP/HSTS) on GET /api/v3/profile",
            "http://127.0.0.1:8779/api/v3/profile",
        ));
        let b = vuln_key_of(&vuln(
            "Security response headers absent on /api/v3/items",
            "http://127.0.0.1:8779/api/v3/items",
        ));
        assert_eq!(a.as_deref(), Some("vulnlike|headers|127.0.0.1:8779"));
        assert_eq!(a, b);
    }

    #[test]
    fn 와일드카드_cors도_클래스가_잡혀_호스트_단위로_모인다() {
        let a = vuln_key_of(&vuln(
            "Wildcard CORS (ACAO: *) on /api/v3/login",
            "http://127.0.0.1:8779/api/v3/login",
        ));
        let b = vuln_key_of(&vuln(
            "Wildcard CORS on authenticated /api/v3/profile",
            "http://127.0.0.1:8779/api/v3/profile",
        ));
        assert_eq!(a.as_deref(), Some("vulnlike|cors|127.0.0.1:8779"));
        assert_eq!(a, b);
    }

    #[test]
    fn 엔드포인트별_취약점은_경로가_다르면_그대로_분리된다() {
        let a = vuln_key_of(&vuln(
            "Path traversal (LFI) on GET /api/v3/file",
            "http://127.0.0.1:8779/api/v3/file",
        ));
        let b = vuln_key_of(&vuln(
            "Reflected XSS on /api/v3/echo",
            "http://127.0.0.1:8779/api/v3/echo",
        ));
        assert_ne!(a, b);
        assert!(a.is_some_and(|k| k.contains("/api/v3/file")));
    }

    #[test]
    fn 호스트_전역_병합은_최고_심각도를_지킨다() {
        let list = vec![
            vuln(
                "Wildcard CORS on static /assets/app.js",
                "http://127.0.0.1:8779/assets/app.js",
            ),
            DedupableFinding {
                severity: "medium".into(),
                ..vuln(
                    "Wildcard CORS on /api/v3/login lets any origin read the admin token",
                    "http://127.0.0.1:8779/api/v3/login — longer body wins primary",
                )
            },
        ];
        let MergeResult {
            merged,
            merged_away,
        } = merge_duplicate_endpoints(list);
        assert_eq!(merged_away, 1);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged.first().map(|m| m.severity.as_str()), Some("medium"));
    }

    // ── mergeDuplicateEndpoints ─────────────────────────────────────────────

    #[test]
    fn 같은_엔드포인트_3개를_1개로_병합하고_근거는_합친다() {
        let list = vec![
            DedupableFinding {
                evidence_ids: strs(&["1"]),
                tags: strs(&["a"]),
                ..f("A", "GET https://x.com/api/comments — 짧은 관측")
            },
            DedupableFinding {
                evidence_ids: strs(&["2"]),
                tags: strs(&["b"]),
                severity: "medium".into(),
                ..f(
                    "B",
                    "x.com — comments (/api/comments)\n훨씬 더 긴 관측 본문이라 이게 primary 가 된다",
                )
            },
            DedupableFinding {
                evidence_ids: strs(&["3"]),
                ..f("C", "https://x.com/api/comments?slug=y 중간")
            },
        ];
        let MergeResult {
            merged,
            merged_away,
        } = merge_duplicate_endpoints(list);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged_away, 2);
        let only = merged.first().cloned().unwrap_or_default();
        assert!(only.body.contains("primary"));
        assert_eq!(sorted(&only.evidence_ids), ["1", "2", "3"]);
        assert_eq!(sorted(&only.tags), ["a", "b"]);
        assert_eq!(only.severity, "medium");
    }

    #[test]
    fn 다른_엔드포인트는_병합하지_않는다() {
        let list = vec![
            f("A", "https://x.com/api/users"),
            f("B", "https://x.com/api/orders"),
        ];
        let MergeResult {
            merged,
            merged_away,
        } = merge_duplicate_endpoints(list);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged_away, 0);
    }

    #[test]
    fn vuln은_클래스가_다르면_같은_url이어도_병합하지_않는다() {
        let list = vec![
            vuln("SQLi in /api/q", "https://x.com/api/q"),
            vuln("IDOR in /api/q", "https://x.com/api/q"),
        ];
        assert_eq!(merge_duplicate_endpoints(list).merged.len(), 2);
    }

    // ── vulnKeyOf — workers that saw the same sink (TimeKORP r5 regression) ──

    #[test]
    fn 같은_클래스_엔드포인트_파라미터의_xss_3건은_같은_키를_낸다() {
        let titles = [
            "Reflected XSS via `format` param — GET http://localhost:1337/ (\"Time\" app)",
            "Reflected XSS: GET localhost:1337/?format= reflects strftime output into HTML unescaped",
            "Reflected XSS in `format` parameter",
        ];
        let bodies = [
            "The `format` query parameter is reflected into the `<h1>`.",
            "**Target:** `GET http://localhost:1337/?format=<value>`.",
            "Reflected XSS confirmed on GET http://localhost:1337/?format=.",
        ];
        let keys: std::collections::HashSet<Option<String>> = titles
            .iter()
            .zip(bodies)
            .map(|(title, body)| {
                vuln_key_of(&DedupableFinding {
                    tags: strs(&["xss"]),
                    ..vuln(title, body)
                })
            })
            .collect();
        assert_eq!(keys.len(), 1);
    }

    #[test]
    fn xss_3건_병합은_1건_남고_severity_최대_validated_승격_근거_합집합() {
        let list = vec![
            DedupableFinding {
                severity: "high".into(),
                status: "validated".into(),
                evidence_ids: strs(&["1"]),
                ..vuln(
                    "Reflected XSS via `format` param",
                    "GET http://localhost:1337/?format= 가 이스케이프 없이 반사 — 관측 A",
                )
            },
            DedupableFinding {
                severity: "high".into(),
                evidence_ids: strs(&["2"]),
                ..vuln(
                    "Reflected XSS: GET http://localhost:1337/?format= unescaped",
                    "관측 B",
                )
            },
            DedupableFinding {
                severity: "medium".into(),
                evidence_ids: strs(&["3"]),
                ..vuln(
                    "Reflected XSS in `format` parameter",
                    "http://localhost:1337/?format=<script> 반사 확인 — 관측 C",
                )
            },
        ];
        let MergeResult {
            merged,
            merged_away,
        } = merge_duplicate_endpoints(list);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged_away, 2);
        let only = merged.first().cloned().unwrap_or_default();
        assert_eq!(only.severity, "high");
        assert_eq!(only.status, "validated");
        assert_eq!(sorted(&only.evidence_ids), ["1", "2", "3"]);
    }

    #[test]
    fn 보안_헤더_누락_candidate와_validated_쌍도_하나로_병합된다() {
        let list = vec![
            DedupableFinding {
                severity: "medium".into(),
                status: "validated".into(),
                ..vuln(
                    "Missing security headers (no CSP / HSTS / X-Frame-Options / nosniff) — http://localhost:1337/",
                    "No Content-Security-Policy (high).",
                )
            },
            DedupableFinding {
                severity: "medium".into(),
                status: "candidate".into(),
                tags: strs(&["headers", "csp"]),
                ..vuln(
                    "localhost:1337 (\"Time\" app) ships no security response headers (CSP/HSTS/XFO/nosniff)",
                    "`http://localhost:1337/` document responses carry none.",
                )
            },
        ];
        let merged = merge_duplicate_endpoints(list).merged;
        assert_eq!(merged.len(), 1);
        assert_eq!(merged.first().map(|m| m.status.as_str()), Some("validated"));
    }

    #[test]
    fn 같은_xss_클래스여도_파라미터가_다르면_별개_발견이다() {
        let a = vuln_key_of(&vuln(
            "XSS in `q` parameter",
            "GET https://x.com/search?q=1",
        ));
        let b = vuln_key_of(&vuln(
            "XSS in `lang` parameter",
            "GET https://x.com/search?lang=en",
        ));
        assert_ne!(a, b);
    }

    #[test]
    fn 클래스를_알_수_없는_vuln은_병합_대상이_아니다() {
        assert_eq!(
            vuln_key_of(&vuln("이상한 동작 관찰", "https://x.com/a")),
            None
        );
    }

    #[test]
    fn auth와_secret_카테고리는_여전히_병합하지_않는다() {
        let auth = |title: &str| DedupableFinding {
            category: "auth".into(),
            ..f(title, "https://x.com/a")
        };
        let list = vec![auth("broken auth"), auth("broken auth again")];
        assert_eq!(merge_duplicate_endpoints(list).merged.len(), 2);
    }

    #[test]
    fn category_other의_헤더_누락_중복도_병합된다() {
        let other = |title: &str, body: &str, severity: &str| DedupableFinding {
            category: "other".into(),
            severity: severity.into(),
            ..f(title, body)
        };
        let list = vec![
            other(
                "Missing security headers on localhost:1337 (no CSP/HSTS/X-Frame-Options/nosniff)",
                "security_inspect on http://localhost:1337/ returned none.",
                "medium",
            ),
            other(
                "Missing HTTP security headers on localhost:1337 app root (no CSP, HSTS, X-Frame-Options, nosniff)",
                "all responses from http://localhost:1337/ omit them.",
                "low",
            ),
        ];
        let merged = merge_duplicate_endpoints(list).merged;
        assert_eq!(merged.len(), 1);
        assert_eq!(merged.first().map(|m| m.severity.as_str()), Some("medium"));
    }

    // ── isScanNoise ─────────────────────────────────────────────────────────

    #[test]
    fn false_positive_상태는_노이즈다() {
        assert!(is_scan_noise::<&str>("false_positive", &[]));
    }

    #[test]
    fn negative_태그는_상태와_무관하게_노이즈다() {
        assert!(is_scan_noise("candidate", &["broken_auth", "negative"]));
        assert!(is_scan_noise("validated", &["Negative"]));
    }

    #[test]
    fn 진짜_후보나_검증_발견은_노이즈가_아니다() {
        assert!(!is_scan_noise("candidate", &["ssrf"]));
        assert!(!is_scan_noise("validated", &["sqli"]));
    }

    // ── mergeDuplicateEndpoints — evidence-based dedup (LockTalk cross-bucket dupes) ──

    #[test]
    fn 같은_request_id를_공유하는_같은_클래스_재진술은_하나로_합친다() {
        let list = vec![
            DedupableFinding {
                severity: "medium".into(),
                status: "validated".into(),
                request_id: Some("75207.72".into()),
                tags: strs(&["access-control-bypass"]),
                ..vuln(
                    "Proxy deny-rule bypass on /api/v1/get_ticket (// prefix)",
                    "ACL bypass via //",
                )
            },
            DedupableFinding {
                severity: "low".into(),
                status: "candidate".into(),
                evidence_ids: strs(&["75207.72"]),
                tags: strs(&["haproxy"]),
                ..vuln(
                    "HAProxy ACL bypass: double-slash reaches /api/v1/get_ticket",
                    "same // technique, longer body so it becomes primary",
                )
            },
        ];
        let MergeResult {
            merged,
            merged_away,
        } = merge_duplicate_endpoints(list);
        assert_eq!(merged_away, 1);
        assert_eq!(merged.len(), 1);
        let only = merged.first().cloned().unwrap_or_default();
        assert_eq!(only.severity, "medium");
        assert_eq!(only.status, "validated");
    }

    #[test]
    fn 같은_request_id라도_서로_다른_알려진_클래스면_합치지_않는다() {
        let list = vec![
            DedupableFinding {
                severity: "medium".into(),
                request_id: Some("75207.72".into()),
                ..vuln(
                    "HAProxy ACL bypass reaches /api/v1/get_ticket",
                    "access-control bypass",
                )
            },
            DedupableFinding {
                severity: "critical".into(),
                evidence_ids: strs(&["75207.72"]),
                ..vuln(
                    "python_jwt token forgery (CVE-2022-39227) on /api/v1/flag",
                    "JWS confusion forges admin",
                )
            },
        ];
        let MergeResult {
            merged,
            merged_away,
        } = merge_duplicate_endpoints(list);
        assert_eq!(merged_away, 0);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn 새_클래스인_acl_경로_정규화와_jwt_위조를_인식한다() {
        let acl = vuln_key_of(&vuln(
            "Proxy deny-rule bypass (path normalization) on http://h/api/v1/get_ticket",
            "",
        ));
        let jwt = vuln_key_of(&vuln(
            "python_jwt token forgery on http://h/api/v1/flag",
            "",
        ));
        assert!(acl.is_some_and(|k| k.contains("|accesscontrol|")));
        assert!(jwt.is_some_and(|k| k.contains("|jwt|")));
    }

    #[test]
    fn 엔드포인트_인벤토리는_vuln과_evidence를_공유해도_흡수되지_않는다() {
        let list = vec![
            DedupableFinding {
                request_id: Some("9.1".into()),
                severity: "high".into(),
                ..vuln("JWT forgery on http://h/api/v1/flag", "x")
            },
            DedupableFinding {
                evidence_ids: strs(&["9.1"]),
                ..f(
                    "http://h/api/v1/flag — JWT-protected flag endpoint",
                    "inventory",
                )
            },
        ];
        assert_eq!(merge_duplicate_endpoints(list).merged.len(), 2);
    }

    // ── vulnKeyOf — path canonicalization (double-slash bypass) ─────────────

    #[test]
    fn 이중_슬래시_경로와_단일_슬래시_경로는_같은_엔드포인트_키다() {
        let a = vuln_key_of(&vuln(
            "HAProxy ACL bypass reaches http://h:1337//api/v1/get_ticket",
            "",
        ));
        let b = vuln_key_of(&vuln(
            "HAProxy ACL bypass reaches http://h:1337/api/v1/get_ticket",
            "",
        ));
        assert_eq!(a, b);
        assert!(a.is_some_and(|k| k.contains("/api/v1/get_ticket")));
    }

    // ── Rust-only: places where the regex/url crates differ from JavaScript ──

    #[test]
    fn 단어_사이의_이모지는_한_글자_와일드카드로_넘지_못한다() {
        // "💉" is two UTF-16 units, so /sql.?inject/ does not match in JavaScript.
        assert_eq!(vuln_key_of(&vuln("SQL💉injection", "http://h/p")), None);
        assert_eq!(
            vuln_key_of(&vuln("SQL→injection", "http://h/p")).as_deref(),
            Some("vulnlike|sqli|h/p|")
        );
    }

    #[test]
    fn url_경로의_캐럿은_퍼센트_인코딩된_표기와_같은_키를_낸다() {
        let raw = endpoint_key_of(&f("A", "http://x.com/a^b{c}|d"));
        let encoded = endpoint_key_of(&f("B", "http://x.com/a%5Eb%7Bc%7D|d"));
        assert_eq!(raw.as_deref(), Some("x.com/a%5Eb%7Bc%7D|d"));
        assert_eq!(raw, encoded);
    }

    // ── Rust-only: the JSON shape that replaces the TypeScript generic ──────

    #[test]
    fn 병합해도_저장소의_다른_필드와_camel_case_이름이_유지된다() {
        let parse = |json: serde_json::Value| serde_json::from_value::<DedupableFinding>(json).ok();
        let list: Vec<DedupableFinding> = [
            parse(serde_json::json!({
                "id": "a", "title": "A", "body": "https://x.com/api/users — the longer body",
                "category": "endpoint", "severity": "low", "status": "candidate", "tags": [],
                "evidenceIds": ["1"], "requestId": "7.1", "createdAt": 20, "origin": "x.com"
            })),
            parse(serde_json::json!({
                "id": "b", "title": "B", "body": "https://x.com/api/users", "category": "endpoint",
                "severity": "low", "status": "candidate", "tags": [], "evidenceIds": [],
                "createdAt": 10, "origin": "other"
            })),
        ]
        .into_iter()
        .flatten()
        .collect();
        assert_eq!(list.len(), 2);
        let result = serde_json::to_value(merge_duplicate_endpoints(list)).unwrap_or_default();
        assert_eq!(
            result,
            serde_json::json!({
                "merged": [{
                    "id": "a", "title": "A", "body": "https://x.com/api/users — the longer body",
                    "category": "endpoint", "severity": "low", "status": "candidate", "tags": [],
                    "evidenceIds": ["1", "7.1"], "requestId": "7.1", "createdAt": 10, "origin": "x.com"
                }],
                "mergedAway": 1
            })
        );
    }
}
