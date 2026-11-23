//! The Red Team scan orchestrator — port of `src/renderer/src/stores/scan.ts`.
//!
//! Phases: auth bootstrap → discovery (same-site links) → per-tab worker pool
//! → endpoint collection → per-endpoint probe pool (probe+ intensity) → the
//! merged report via `finding_export`. Workers are their own Claude CLI
//! sessions (MAX_PARALLEL at a time), pumped from the app's wake loop; the
//! scan never blocks the interface. Not ported here (v1): the alternate-tenant
//! bootstrap, the escalation phase, and the pre-scan findings/cookie wipe.

use std::collections::VecDeque;

use rever_chat_core::acp_permission::{RequestPermissionOutcome, RequestPermissionResponse};
use rever_scope::fence::is_loopback_host;
use rever_scope::scope::scope_key;
use serde_json::Value;

use crate::agent::{AgentEvent, AgentSession};

/// The TypeScript pool cap (`MAX_PARALLEL = 8`).
const MAX_PARALLEL: usize = 8;
const LOG_CAP: usize = 500;

// ── Parsers (pure) ───────────────────────────────────────────────────────────

/// Static, non-navigable assets never become page tabs (`NON_PAGE_ASSET`).
fn is_non_page_asset(url: &str) -> bool {
    let bare = url.split(['?', '#']).next().unwrap_or(url);
    let Some(dot) = bare.rfind('.') else {
        return false;
    };
    matches!(
        &bare[dot + 1..],
        "ttf" | "otf" | "woff" | "woff2" | "eot" | "css" | "js" | "mjs" | "cjs" | "map"
            | "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "avif" | "ico" | "bmp"
            | "mp4" | "webm" | "mp3" | "wav" | "ogg" | "m4a" | "mov" | "pdf" | "zip"
            | "gz" | "wasm"
    )
}

/// The first JSON array in the text (the discovery/collector turns print one).
fn json_array(text: &str) -> Option<Vec<Value>> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    let array: Vec<Value> = serde_json::from_str(&text[start..=end]).ok()?;
    Some(array)
}

/// Same-site absolute page URLs from the discovery turn's JSON array.
pub fn parse_links(text: &str, site: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in json_array(text).unwrap_or_default() {
        let Some(url) = item.as_str() else { continue };
        if !url.starts_with("http://") && !url.starts_with("https://") {
            continue;
        }
        if scope_key(url).as_deref() != Some(site) || !seen.insert(url.to_owned()) {
            continue;
        }
        if is_non_page_asset(url) {
            continue;
        }
        out.push(url.to_owned());
        if out.len() >= cap {
            break;
        }
    }
    out
}

/// "METHOD URL" endpoint strings from the collector turn, same-site, deduped
/// by bare path.
pub fn parse_endpoints(text: &str, site: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in json_array(text).unwrap_or_default() {
        let Some(x) = item.as_str() else { continue };
        let x = x.trim();
        let (method, url) = match x.split_once(' ') {
            Some((m, u)) if m.len() <= 8 && m.chars().all(|c| c.is_ascii_uppercase()) => {
                (m.to_owned(), u.to_owned())
            }
            _ => ("GET".to_owned(), x.to_owned()),
        };
        if !url.starts_with("http://") && !url.starts_with("https://") {
            continue;
        }
        if scope_key(&url).as_deref() != Some(site) {
            continue;
        }
        let bare = url.split(['?', '#']).next().unwrap_or(&url).to_owned();
        if !seen.insert(bare) {
            continue;
        }
        out.push(format!("{method} {url}"));
        if out.len() >= cap {
            break;
        }
    }
    out
}

fn path_of(url: &str) -> String {
    url::Url::parse(url)
        .map(|parsed| parsed.path().to_owned())
        .unwrap_or_else(|_| url.split(['?', '#']).next().unwrap_or(url).to_owned())
}

/// A short log tag for a page URL: its last path segment (the host when the
/// path is empty), so worker lines read `[items]` instead of the full URL.
fn short_tag(url: &str) -> String {
    // The URL parser percent-encodes non-ASCII; undo the hex pairs for display.
    let decode = |text: &str| {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] == b'%' && at + 3 <= bytes.len() {
                let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).unwrap_or("");
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    at += 3;
                    continue;
                }
            }
            out.push(bytes[at]);
            at += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    let parsed = url::Url::parse(url).ok();
    let segment = parsed
        .as_ref()
        .and_then(|url| url.path().split('/').rfind(|s| !s.is_empty()))
        .map(decode);
    let tag = segment
        .or_else(|| {
            parsed
                .map(|url| url.host_str().unwrap_or_default().to_owned())
        })
        .unwrap_or_else(|| url.split(['?', '#']).next().unwrap_or(url).to_owned());
    let mut chars: Vec<char> = tag.chars().collect();
    if chars.len() > 15 {
        chars.truncate(15);
        chars.push('…');
    }
    chars.into_iter().collect()
}

/// A discovered page URL carrying a query supersedes its bare GET — a query
/// endpoint generates the reflection/injection checklist, a bare one does not.
pub fn merge_query_endpoints(endpoints: Vec<String>, links: &[String], cap: usize) -> Vec<String> {
    let mut by_path: std::collections::HashMap<String, Vec<String>> = Default::default();
    let mut order: Vec<String> = Vec::new();
    for entry in endpoints {
        let url = entry.split(' ').nth(1).unwrap_or(&entry).to_owned();
        let key = path_of(&url);
        if !by_path.contains_key(&key) {
            order.push(key.clone());
        }
        by_path.entry(key).or_default().push(entry);
    }
    for url in links {
        if !url.contains('?') {
            continue;
        }
        let key = path_of(url);
        let list = by_path.entry(key.clone()).or_default();
        if list.iter().any(|entry| entry.starts_with("GET ") && entry.contains('?')) {
            continue;
        }
        let replacement = format!("GET {url}");
        if let Some(at) = list
            .iter()
            .position(|entry| entry.starts_with("GET ") && !entry.contains('?'))
        {
            list[at] = replacement;
        } else {
            list.push(replacement);
        }
    }
    order
        .iter()
        .flat_map(|key| by_path[key].clone())
        .take(cap)
        .collect()
}

/// Hosts the app actually talked to on this site's connections — the store
/// opens the follow-up scope for them deterministically.
pub fn connected_hosts(requests: &[String], site: &str) -> Vec<String> {
    let mut hosts = std::collections::BTreeSet::new();
    for url in requests {
        if scope_key(url).as_deref() != Some(site) {
            continue;
        }
        if let Some(host) = url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split(['/']).next())
            .map(str::to_owned)
        {
            hosts.insert(host);
        }
    }
    hosts.into_iter().collect()
}

/// The first JSON array line of `[\"METHOD URL\", …]` for scope_add.
pub fn scope_add_prompt(hosts: &[String]) -> String {
    format!(
        "scope_add({{ hosts: {} }}) 로 이 앱이 연결된 호스트를 범위에 추가한다. 다른 설명 없이 호출만.",
        serde_json::to_string(hosts).unwrap_or_default()
    )
}

// ── Per-endpoint checklist (port of ai/redteam-categories.ts) ────────────────

struct VulnCheck {
    id: &'static str,
    owasp: &'static str,
    severity: &'static str,
    check: &'static str,
}

const CHECKS: &[VulnCheck] = &[
    VulnCheck {
        id: "broken_auth",
        owasp: "API2:Broken Authentication",
        severity: "high",
        check: "noauth_probe({ requestId }) 를 1회 실행한다 — 캡처 요청을 raw 소켓으로 (1) 오너 세션(파티션에서 자동) (2) 쿠키 없이 두 번 보내, 쿠키 없이도 같은 보호 리소스가 오면 인증 우회 확정(high). replay_request 로 Cookie 를 비워도 httpOnly 세션은 credentials:include 로 재첨부되어 \"무인증 200\" 오탐이 나므로, 반드시 noauth_probe 를 쓴다. loopback/lab 게이트.",
    },
    VulnCheck {
        id: "idor",
        owasp: "API1:BOLA/IDOR",
        severity: "high",
        check: "객체 id(경로 UUID/숫자 또는 쿼리 id)로 접근하는 인증 리소스면 idor_probe({ requestId }) 를 1회 실행한다 — 대체 테넌트 세션으로 소유자 요청을 재생해, 익명은 거부되는데 다른 테넌트가 같은 리소스를 읽으면 BOLA/IDOR 확정(high/critical). 익명도 같은 리소스를 주면 그건 BOLA 가 아니라 공개 노출(broken auth)이니 그렇게 기록.",
    },
    VulnCheck {
        id: "xss",
        owasp: "WSTG-INPV:Reflected XSS",
        severity: "medium",
        check: "반사되는 파라미터(쿼리 또는 폼 바디)를 § 로 표시하고 payload_probe({ requestId }) 로 반사형 XSS 를 확인한다. 응답에 페이로드가 이스케이프 없이 반사되면 보고.",
    },
    VulnCheck {
        id: "ssti",
        owasp: "WSTG-INPV:Server-Side Template Injection",
        severity: "high",
        check: "ssti_probe({ requestId, param }) 를 1회 실행한다 — Velocity/Freemarker/Jinja/ERB/Thymeleaf/Razor 를 한 번에 시험한다. 한 엔진이라도 산술을 평가하면 SSTI 확정: severity:\"critical\" 로 올려 보고한다. {{7*7}}/${7*7} 리터럴 반사는 기각 근거가 아니다.",
    },
    VulnCheck {
        id: "cmdi",
        owasp: "WSTG-INPV:OS Command Injection",
        severity: "critical",
        check: "cmd_probe({ requestId, param }) 를 1회 실행한다 — quote break/;/|/backtick/$() 컨텍스트를 한 번에 시험한다. 셸이 nonce 산술을 계산해 돌려보내면 커맨드 인젝션 확정: RCE 급.",
    },
    VulnCheck {
        id: "sqli",
        owasp: "WSTG-INPV:SQL Injection",
        severity: "critical",
        check: "sqli_probe({ requestId, param }) 를 1회 실행한다 — quote 에러 시그니처와 boolean 산술 차등을 한 번에 시험한다. SQL 에러 문자열 또는 true/false 응답 차등이면 SQL 인젝션 확정: critical.",
    },
    VulnCheck {
        id: "nosqli",
        owasp: "WSTG-INPV:NoSQL / Cypher Injection",
        severity: "critical",
        check: "nosqli_probe({ requestId, param }) 를 1회 실행한다 — 백엔드가 Mongo/Neo4j 로 보이면 sqli_probe 대신 이것을 쓴다. param[$ne] vs param[$eq] 차등과 파서 에러 시그니처로 판정한다.",
    },
    VulnCheck {
        id: "pathwrite",
        owasp: "WSTG-ATHZ:Path Traversal / Arbitrary File Write",
        severity: "critical",
        check: "저장-후-표시형 폼에서 값이 서버 파일명이 될 만한 필드(name/customer/filename/title 류)를 param 으로 path_write_probe({ requestId, param }) 를 1회 실행한다. 평문 이름 마커는 뜨는데 ../ 이름 마커는 안 뜨면 임의 파일 쓰기 확정: critical.",
    },
    VulnCheck {
        id: "crlf",
        owasp: "WSTG-INPV:CRLF/Header Injection",
        severity: "medium",
        check: "crlf_test({ requestId }) 로 헤더 주입을 1회 확인한다.",
    },
    VulnCheck {
        id: "ssrf",
        owasp: "API7:SSRF / Open Redirect",
        severity: "high",
        check: "URL 류 파라미터에 내부 주소(http://169.254.169.254/ 또는 http://localhost/)를 넣어 replay_request 로 1회 재생한다. 서버가 그 주소를 가져오거나 리다이렉트하면 SSRF/오픈리다이렉트.",
    },
    VulnCheck {
        id: "lfi",
        owasp: "WSTG-ATHZ:LFI/Path Traversal",
        severity: "high",
        check: "파일/경로 파라미터가 § 마커로 지정되면 lfi_probe 로 경로 traversal 을 1회 확인한다.",
    },
];

fn is_url_param(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "url" | "uri" | "redirect" | "redirect_uri" | "return" | "return_url" | "next"
            | "callback" | "dest" | "destination" | "target" | "continue" | "feed" | "image"
            | "img" | "webhook" | "link" | "site" | "domain" | "host" | "path" | "file"
            | "page" | "load" | "src"
    )
}

fn is_file_param(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "file" | "filename" | "path" | "template" | "page" | "doc" | "document" | "include"
            | "view" | "download" | "read" | "load"
    )
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.as_bytes()
            .iter()
            .enumerate()
            .all(|(at, b)| matches!(at, 8 | 13 | 18 | 23) && *b == b'-' || b.is_ascii_hexdigit())
}

fn query_pairs(url: &str) -> Vec<(String, String)> {
    url::Url::parse(url)
        .map(|parsed| {
            parsed
                .query_pairs()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn has_id_param(url: &str) -> bool {
    let segs_have_id = path_of(url)
        .split('/')
        .filter(|s| !s.is_empty())
        .any(|s| s.chars().all(|c| c.is_ascii_digit()) || is_uuid(s));
    if segs_have_id {
        return true;
    }
    query_pairs(url).iter().any(|(k, v)| {
        let key = k.to_ascii_lowercase();
        let key_id = |name: &str| {
            name.split('_')
                .next_back()
                .is_some_and(|last| matches!(last, "id" | "uid" | "uuid" | "user" | "account" | "order" | "pid" | "no" | "seq"))
        };
        key_id(&key)
            || v.chars().all(|c| c.is_ascii_digit())
            || is_uuid(v)
    })
}

/// Which active checks apply to one endpoint, by the cheap URL heuristics of
/// the TypeScript `classify`.
pub fn checklist_for(method: &str, url: &str) -> String {
    let params = query_pairs(url);
    let names: Vec<&str> = params.iter().map(|(k, _)| k.as_str()).collect();
    let body_method = matches!(method.to_ascii_uppercase().as_str(), "POST" | "PUT" | "PATCH");
    let reflectable = !names.is_empty() || body_method;
    let path = path_of(url);
    let path_names: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    let mut picked: Vec<&VulnCheck> = Vec::new();
    let push = |picked: &mut Vec<&'static VulnCheck>, id: &str| {
        if let Some(check) = CHECKS.iter().find(|c| c.id == id) {
            picked.push(check);
        }
    };
    push(&mut picked, "broken_auth");
    if has_id_param(url) {
        push(&mut picked, "idor");
    }
    if reflectable {
        for id in ["xss", "ssti", "cmdi", "sqli", "nosqli"] {
            push(&mut picked, id);
        }
    }
    if reflectable && (names.len() >= 2 || body_method) {
        push(&mut picked, "pathwrite");
    }
    if !names.is_empty() {
        push(&mut picked, "crlf");
    }
    if names.iter().any(|name| is_url_param(name)) {
        push(&mut picked, "ssrf");
    }
    if names.iter().any(|name| is_file_param(name))
        || path_names.iter().any(|seg| is_file_param(seg))
    {
        push(&mut picked, "lfi");
    }
    if picked.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = picked
        .into_iter()
        .map(|check| {
            format!(
                "- [{}] {} 발견 시 finding_add(category:\"vuln\", severity:\"{}\", tags:[\"{}\"]).",
                check.owasp, check.check, check.severity, check.id
            )
        })
        .collect();
    format!("적용 취약점 카테고리(해당되는 것만, 각 단발):\n{}", lines.join("\n"))
}

// ── Prompts ──────────────────────────────────────────────────────────────────

/// Throwaway credentials the auth bootstrap registers with — generated here
/// so the agent fills exactly these, not invented ones.
fn throwaway_user() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let noise = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|at| at.subsec_nanos())
        .unwrap_or_default();
    format!("rever_{noise:x}")
}

fn throwaway_pass() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let noise = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|at| at.as_nanos() % 0x100_0000)
        .unwrap_or_default();
    format!("Rv_{noise:x}!1")
}

pub fn auth_prompt(target: &str, user: &str, pass: &str) -> String {
    format!(
        "browser_navigate(\"{target}\") 후 browser_snapshot. 이 페이지가 로그인/회원가입 게이트면(로그인 폼이 있거나 /login·/panel 로 리다이렉트), 스캔을 인증 상태로 만들기 위해 먼저 계정을 만든다: 회원가입(register/signup) 링크나 폼으로 가서 browser_type 으로 사용자명 \"{user}\" 와 비밀번호 \"{pass}\" 를 넣고 제출한 뒤, 로그인 폼에 같은 자격으로 로그인한다. 폼 필드명이 username/password 가 아니면 스냅샷의 실제 필드명을 쓰고, 이메일 필드가 있으면 \"{user}@example.com\" 을 넣는다. 최대 6번의 browser 액션. 로그인에 성공해 보호 영역으로 들어가면 'AUTHED' 한 단어만, 인증 게이트가 없거나 로그인에 실패하면 'PUBLIC' 한 단어만 출력."
    )
}

/// Phase 0b — the attacker tenant. Stashed via redteam_alt_session_set so
/// idor_probe can cross-check; the owner session is restored afterwards so
/// discovery crawls the data-rich tenant.
pub fn alt_tenant_prompt(origin: &str, user: &str, pass: &str) -> String {
    format!(
        "BOLA/IDOR 를 확정하려면 2번째(공격자) 테넌트가 필요하다. 순서대로: 1) cookie_list({{ urls: [\"{origin}\"] }}) 로 현재 소유자 세션 쿠키(이름=값들)를 읽어 전부 기억한다. 2) 회원가입(register/signup) 폼으로 가서 새 계정 사용자명 \"{user}\", 비밀번호 \"{pass}\" (이메일 필드가 있으면 \"{user}@example.com\")로 가입하고 로그인한다. 3) cookie_list({{ urls: [\"{origin}\"] }}) 로 이 새 계정의 세션 쿠키를 읽어, 전체를 \"name1=value1; name2=value2\" 형식으로 합쳐 redteam_alt_session_set({{ cookie: \"<합친 값>\" }}) 로 저장한다. 4) 1)에서 기억한 소유자 쿠키를 각각 cookie_set({{ name, value, url: \"{origin}\", httpOnly: true }}) 로 복원해 원래(데이터 있는) 세션으로 되돌린다 — 이후 크롤이 소유자로 돌아야 한다. 저장까지 성공하면 'ALT_READY' 한 단어만, 회원가입이 불가하거나 실패하면 'NO_ALT' 한 단어만 출력."
    )
}

/// Phase 3.7 — prove impact by chaining confirmed vulns (disposable targets).
pub fn escalate_prompt() -> String {
    "에스컬레이션 단계 — 확인된 취약점의 최종 임팩트를 증명한다. 먼저 finding_list(category:\"vuln\") 로 severity critical/high 인 항목을 최대 5개 고른다(임팩트 태그가 이미 붙은 것은 건너뛴다). 각각에 대해 취약점 종류를 넘어 체인을 실행한다: SSTI 면 템플릿·플래그·환경변수 파일 읽기, 경로 쓰기(path_write)면 템플릿/소스 덮어쓰기 후 렌더 확인, SQLi 면 실제 데이터 한 행 추출(UNION 또는 에러 기반), BOLA 면 타 테넌트 리소스 본문 인용, broken auth 면 인증 없는 접근으로 보호 데이터 확보. 도구는 ssti_probe/replay_request/raw_http/grpc_call/pickle_gadget 등 프로브 프로파일 전부 허용. 이 타겟은 일회성(사용자 소유)이므로 파일 쓰기·삭제를 포함한 파괴적 행동도 허용된다. 임팩트가 증명되면 finding_add(category:\"vuln\", severity:\"critical\", tags:[\"impact\"]) 로 원본 finding id 를 evidenceIds 에 넣어 기록하고, 증명에 실패하면 새로 기록하지 않는다. 끝나면 증명된 임팩트 개수만 한 줄로 답한다.".to_owned()
}

pub fn discovery_prompt(target: &str, scope_host: &str, pages: u32) -> String {
    format!(
        "scope_set({{ hosts: [\"{scope_host}\"] }}) 로 범위를 고정하고 browser_navigate(\"{target}\") 후 browser_snapshot. 이후 이 타겟 탭에서는 다른 페이지로 이동하지 말 것 — browser_navigate 로 벗어나지 않는다. 점검할 다른 페이지는 스캔이 각각 새 탭으로 열어 워커가 거기서 더 깊이 탐색한다. list_requests 로 이 앱이 호출하는 자체 API 호스트(예: 다른 localhost 포트의 백엔드, api.* 서브도메인)를 확인하면 scope_add 로 범위에 추가한다 — 단 서드파티(분석/광고/CDN)는 추가하지 않는다. 랜딩 스냅샷과 list_requests 에 보이는 같은 사이트 링크/경로를 점검할 만한 페이지로 최대 {pages}개 모은다. 설명 없이 절대경로 URL들의 JSON 배열 한 줄만 출력."
    )
}

pub fn worker_prompt(url: &str, crawl: bool, aggressive: bool) -> String {
    let crawl_line = if crawl { "- 먼저 ui_crawl({tab:N}) 를 한 번 호출한다 — 페이지의 모든 버튼/링크를 결정적으로 눌러 숨은 API 를 드러내고 버튼→API 맵을 만든다(펜스가 변이 요청을 204 로 막아 서버는 안전). 반환된 'endpoints discovered' 가 캡처되어 이후 점검 대상에 포함된다. 그 다음 아래를 진행한다.\n" } else { "" };
    let rules_line = if aggressive {
        "- 오직 rever 도구만 사용한다. Terminal/Bash/셸/curl/wget 및 ToolSearch 금지(scope 를 우회한다). 다른 탭 열기/닫기/선택 금지. 이 타겟은 일회성(사용자 소유)이므로 제출·삭제·결제 컨트롤을 포함한 모든 조작이 허용된다 — 숨은 API 를 드러내는 버튼은 주저 없이 눌러라.\n"
    } else {
        "- 오직 rever 도구만 사용한다. Terminal/Bash/셸/curl/wget 및 ToolSearch 금지(scope 를 우회한다). 다른 탭 열기/닫기/선택 금지. buy/pay/delete/결제/삭제/제출 확인 컨트롤 클릭 금지. 로그인 페이지는 기록만.\n"
    };
    format!(
        "여러 탭이 열려 있다. browser_tabs({{action:\"list\"}}) 로 목록을 보고 URL 이 \"{url}\" 인 탭의 인덱스 N 을 찾는다. 그 탭만 다룬다:\n{crawl_line}\
- 읽기: list_endpoints({{tab:N}}), list_requests({{tab:N}}), 흥미로운 요청은 get_request/get_request_initiator, security_inspect({{url:\"{url}\"}})\n\
- 동적 탐색(필요할 때): browser_snapshot({{tab:N}}) → browser_click/browser_type/browser_navigate 에 같은 tab:N 을 전달한다(아코디언 펼치기, 페이지네이션, 더보기, 검색). ref 는 탭별 네임스페이스라 같은 tab:N 으로만 동작한다.\n{rules_line}\
- finding_add 는 소형 템플릿: 한 줄 제목(엔드포인트/이슈), 관측 불릿 3개 이하, 재현 curl 한 줄. 이미 finding_list 에 같은 엔드포인트가 있으면 추가하지 않는다.\n\
- 정밀도 규칙(중요): ①클라이언트에 보호장치가 \"안 보인다\"는 이유만으로 서버 취약점을 기록하지 않는다 — 실제로 재현/실증했을 때만 category:\"vuln\". ②단순 엔드포인트·인증흐름 목록은 category:\"endpoint\" 로만 적는다(정찰 자료, vuln 아님). ③\"공개가 설계상 정상\" 같은 음성 결론은 기록하지 않는다. ④실증 못 했으면 status 는 candidate 로 두고 본문에 \"미확인: ~ 검증 필요\"라고 적는다. ⑤세션 Set-Cookie 의 Domain 이 여러 환경이 공유하는 상위 도메인이면 기록한다 — dev 토큰이 운영에도 전송된다(tag: shared-cookie-scope).\n\
- 끝나면 발견 개수만 한 줄로 답한다."
    )
}

pub fn probe_collect_prompt() -> String {
    "finding_list · list_endpoints · list_requests 로 지금까지 기록된 엔드포인트를 중복 없이 모은다(같은 path 는 하나. 단, 쿼리 파라미터가 있는 엔드포인트는 파라미터 이름별 대표 URL 을 쿼리째 유지 — 쿼리가 사라지면 워커의 반사/주입 체크리스트가 아예 생성되지 않는다). HTTP 메서드를 아는 경우 \"METHOD URL\" 형식으로, 모르면 \"GET URL\". 최대 30개. 설명 없이 JSON 문자열 배열 한 줄만 출력.".to_owned()
}

pub fn path_probe_prompt(origin: &str, site: &str) -> String {
    format!(
        "path_probe({{ baseUrl: \"{origin}\" }}) 를 한 번 호출해 숨은 경로를 스윕한다(기본 워드리스트: .env/.env.production, admin/·administrator/·wp-admin/, .git/HEAD·.git/config, *.js.map 소스맵, server-status, phpinfo.php, .well-known/security.txt 등). 200 또는 비기본(비정상) 응답을 돌려주는 경로만 finding_add 로 기록한다 — 민감 파일 노출/관리자 페이지/소스맵 누출을 심각도와 함께. 이미 finding_list 에 있으면 추가 금지. 범위({site}) 밖은 건드리지 않는다. 끝나면 발견 개수만 한 줄."
    )
}

pub fn report_prompt(target: &str) -> String {
    format!(
        "지금까지 기록된 finding 으로 finding_export({{ site: \"{target}\" }}) 를 한 번 호출한다. 반환 보고서는 앱이 직접 캡처하므로 본문을 다시 출력하지 말고 \"완료\"라고만 한 줄로 답한다."
    )
}

pub fn level_line(intensity: &str, aggressive: bool) -> String {
    if intensity == "probe" {
        return "강도: probe — replay_request 는 캡처된 요청을 변경 없이 재생만 한다. payload_probe/path_probe/crlf_test 금지. security_inspect/scan_secrets/replay_request(변경 없음)만 사용.".to_owned();
    }
    if aggressive {
        "강도: redteam(일회성 타겟) — 각 체크에서 직전 응답(에러 메시지·반사 형태·상태코드 차이)을 근거로 인코딩/문맥/페이로드를 바꿔 최대 5회까지 적응 재시도할 수 있다. 워드리스트·대량 열거·레이트리밋 재시도는 여전히 금지.".to_owned()
    } else {
        "강도: redteam — 각 체크마다 변형 요청 최대 1개까지 허용.".to_owned()
    }
}

/// The base probe rules — `ai/redteam-probe.md`, condensed to what the pool
/// worker needs (the full per-tool guidance lives in the tools' descriptions).
pub const PROBE_PROMPT: &str = "Red Team 단일 엔드포인트 활성 프로브 — 승인된 사용자 소유 타겟 스윕의 프로브 워커다. 아래 엔드포인트 하나와 강도를 받는다. 기본 단발 요청(체크당 1~2개)으로만 프로브하고, 유지된 것과 아닌 것을 기록한다. 부하 테스트도 퍼징 캠페인도 아니다.\n\n허용: security_inspect, replay_request(강도가 허용하면 변형 1회 포함), ssti_probe, cmd_probe, sqli_probe, nosqli_probe, path_write_probe, noauth_probe, idor_probe, payload_probe, path_probe, crlf_test, scan_secrets — 각 도구의 설명을 따른다.\n\n금지: intruder_run, burst_send(수동 전용), Terminal/Bash/셸/curl/wget/ToolSearch, 체크당 요청 2개 초과(강도 문구가 허용할 때만 적응 재시도, 응답 근거 필수), 사이트 범위 밖의 모든 것, 파괴 경로 반복(확정 요청 1개면 충분).\n\n기록: 확정된 이슈만 finding_add(severity, 증명한 정확한 요청, 재현 한 줄). 응답이 실증했을 때만 validated. 클라이언트 통제 부재로 추정한 서버 약점은 vuln 금지 — candidate 로 두고 미확인 명시. 단순 목록은 category:\"endpoint\". 공개가 설계상 정상이면 기록하지 않는다. 한 줄 제목 / 관측 불릿 3개 이하 / 재현. 끝나면 추가한 발견 개수만 한 줄.";

// ── Orchestrator ─────────────────────────────────────────────────────────────

/// One line of the scan log, as the dialog renders it.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanLine {
    pub kind: LineKind,
    pub text: String,
    /// The tab (or worker) the line came from, shown as `[N]`.
    pub tab: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Phase,
    Tool,
    Finding,
    Error,
}

/// A phase's turn outcome, distilled.
struct TurnOutcome {
    final_text: String,
    finding_adds: u32,
    export: Option<String>,
}

struct Turn {
    session: AgentSession,
    label: Option<String>,
    outcome: TurnOutcome,
    failed: Option<String>,
    done: bool,
}

impl Turn {
    /// Spawn, send the prompt, and add the turn to the pool.
    fn spawn_into(
        pool: &mut Vec<Turn>,
        spawn: &mut Spawner,
        prompt: &str,
        label: Option<String>,
    ) -> Result<(), String> {
        let mut session = spawn(prompt)?;
        let _ = session.send(prompt);
        pool.push(Turn {
            session,
            label,
            outcome: TurnOutcome {
                final_text: String::new(),
                finding_adds: 0,
                export: None,
            },
            failed: None,
            done: false,
        });
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
enum Phase {
    Auth,
    /// The attacker tenant for BOLA cross-checks (redteam only, authed).
    AltTenant,
    Discovery,
    ScopeExpand,
    Workers,
    ProbeCollect,
    ProbeWorkers,
    /// Prove impact by chaining confirmed vulns (redteam, disposable targets).
    Escalate,
    Report,
    Done,
    Failed(String),
}

/// What the app must do for the scan between pumps.
pub enum ScanAction {
    /// Open a background tab for a discovered page.
    OpenTab(String),
    /// The scan finished; close the tabs it opened for its pages.
    CloseScanTabs,
}

pub struct ScanRun {
    target: String,
    pages: u32,
    /// "readonly" | "probe" | "full" (full = the original "redteam").
    intensity: String,
    aggressive: bool,
    phase: Phase,
    /// The sequential phase's turn (auth/discovery/scope/collector/report).
    turn: Option<Turn>,
    /// The parallel pool (workers / endpoint probes).
    pool: Vec<Turn>,
    queue: VecDeque<String>,
    links: Vec<String>,
    opened: Vec<String>,
    pub findings: u32,
    pub report: String,
    pub log: Vec<ScanLine>,
    stop: bool,
}

/// How the orchestrator spawns sessions: (prompt, label) → a live session.
pub type Spawner<'a> = dyn FnMut(&str) -> Result<AgentSession, String> + 'a;

impl ScanRun {
    pub fn new(target: &str, pages: u32, intensity: &str) -> Self {
        let aggressive = is_loopback_host(target);
        Self {
            target: target.trim().to_owned(),
            pages,
            intensity: intensity.to_owned(),
            aggressive,
            phase: Phase::Auth,
            turn: None,
            pool: Vec::new(),
            queue: VecDeque::new(),
            links: Vec::new(),
            opened: Vec::new(),
            findings: 0,
            report: String::new(),
            log: Vec::new(),
            stop: false,
        }
    }

    pub fn running(&self) -> bool {
        !matches!(self.phase, Phase::Done | Phase::Failed(_))
    }

    pub fn stop(&mut self) {
        self.stop = true;
    }

    fn log(&mut self, kind: LineKind, text: impl Into<String>, tab: Option<String>) {
        self.log.push(ScanLine {
            kind,
            text: text.into(),
            tab,
        });
        if self.log.len() > LOG_CAP {
            let cut = self.log.len() - LOG_CAP;
            self.log.drain(..cut);
        }
    }

    fn drain_turn(&mut self, turn: &mut Turn) {
        for event in turn.session.drain() {
            match event {
                AgentEvent::Text(text) => turn.outcome.final_text.push_str(&text),
                AgentEvent::ToolCall(name, input) => {
                    self.log(LineKind::Tool, name.clone(), turn.label.clone());
                    if name.ends_with("finding_add") {
                        turn.outcome.finding_adds += 1;
                        let title = input
                            .get("title")
                            .and_then(Value::as_str)
                            .or_else(|| input.get("name").and_then(Value::as_str))
                            .unwrap_or("");
                        if !title.is_empty() {
                            self.log(LineKind::Finding, title, turn.label.clone());
                        }
                    }
                }
                AgentEvent::ToolResult(name, output) => {
                    if name.ends_with("finding_export") {
                        turn.outcome.export = Some(output);
                    }
                }
                // A worker has nobody to ask: what its own rules do not allow is refused.
                AgentEvent::Permission(ask) => (ask.answer)(RequestPermissionResponse {
                    outcome: RequestPermissionOutcome::Cancelled,
                }),
                AgentEvent::Done => turn.done = true,
                AgentEvent::Failed(error) => {
                    turn.failed = Some(error);
                    turn.done = true;
                }
            }
        }
    }

    /// Advance the scan one tick. `spawn` creates a CLI session for a prompt;
    /// `captured_urls` are the URLs in the traffic store (for scope follow-up).
    /// Returns actions the app must run.
    pub fn pump(&mut self, spawn: &mut Spawner, captured_urls: &[String]) -> Vec<ScanAction> {
        let mut actions = Vec::new();
        if !self.running() {
            return actions;
        }
        // 1. Drain everyone. Events route into `self`, so each drained turn
        // is taken out and put back.
        for turn in std::mem::take(&mut self.pool) {
            let mut turn = turn;
            self.drain_turn(&mut turn);
            self.pool.push(turn);
        }
        if let Some(mut turn) = self.turn.take() {
            self.drain_turn(&mut turn);
            self.turn = Some(turn);
        }

        // 2. A stop request jumps to the report (keeping what was found);
        //    never twice — the report phase owns the turn from then on.
        if self.stop && self.phase != Phase::Report {
            self.pool.clear();
            self.turn = None;
            self.queue.clear();
            self.log(LineKind::Phase, "중지 요청 — 수집된 것으로 보고서를 만든다", None);
            self.start_turn(spawn, &report_prompt(&self.target));
            self.phase = Phase::Report;
            return actions;
        }

        // 3. The very first pump starts the auth bootstrap.
        if self.phase == Phase::Auth && self.turn.is_none() && self.pool.is_empty() {
            self.log(LineKind::Phase, "인증 부트스트랩", None);
            self.start_turn(spawn, &auth_prompt(&self.target, &throwaway_user(), &throwaway_pass()));
            return actions;
        }

        // 4. Phase transitions.
        let turn_done = self.turn.as_ref().is_some_and(|turn| turn.done);
        match &self.phase {
            Phase::Auth if turn_done => {
                let text = self.take_turn_text();
                let authed = text.contains("AUTHED") && !text.contains("PUBLIC");
                self.log(
                    LineKind::Phase,
                    if authed { "인증 세션 확보됨" } else { "인증 게이트 없음 (공개 스캔)" },
                    None,
                );
                if authed && self.intensity == "full" {
                    // A second identity is what confirms BOLA/IDOR: anonymous is
                    // denied, but can a different tenant read the owner's object?
                    self.log(LineKind::Phase, "대체 테넌트 등록", None);
                    self.start_turn(spawn, &alt_tenant_prompt(&self.origin(), &throwaway_user(), &throwaway_pass()));
                    self.phase = Phase::AltTenant;
                } else {
                    self.start_discovery(spawn);
                }
            }
            Phase::AltTenant if turn_done => {
                let text = self.take_turn_text();
                let ready = text.contains("ALT_READY") && !text.contains("NO_ALT");
                self.log(
                    LineKind::Phase,
                    if ready {
                        "공격자 테넌트 준비됨 — idor_probe 교차 검증 가능"
                    } else {
                        "대체 테넌트 없음 (IDOR 은 후보로 남음)"
                    },
                    None,
                );
                self.start_discovery(spawn);
            }
            Phase::Discovery if turn_done => {
                let text = self.take_turn_text();
                let site = scope_key(&self.target).unwrap_or_default();
                self.links = parse_links(&text, &site, self.pages as usize);
                self.log(
                    LineKind::Phase,
                    format!("검사할 페이지 {}개", self.links.len()),
                    None,
                );
                // Deterministic scope follow: hosts the app really talked to.
                let hosts = connected_hosts(captured_urls, &site);
                if !hosts.is_empty() {
                    self.log(LineKind::Phase, format!("범위 확장: {}", hosts.join(", ")), None);
                    self.start_turn(spawn, &scope_add_prompt(&hosts));
                    self.phase = Phase::ScopeExpand;
                } else {
                    self.enter_workers(spawn, &mut actions);
                }
            }
            Phase::ScopeExpand if turn_done => {
                self.take_turn_text();
                self.enter_workers(spawn, &mut actions);
            }
            Phase::Workers => {
                self.pump_pool(spawn, &mut actions);
                if self.pool.is_empty() && self.queue.is_empty() {
                    if self.intensity == "readonly" {
                        self.start_report(spawn);
                    } else {
                        self.log(LineKind::Phase, "엔드포인트 수집", None);
                        self.start_turn(spawn, &probe_collect_prompt());
                        self.phase = Phase::ProbeCollect;
                    }
                }
            }
            Phase::ProbeCollect if turn_done => {
                let text = self.take_turn_text();
                let site = scope_key(&self.target).unwrap_or_default();
                let mut endpoints = parse_endpoints(&text, &site, 30);
                endpoints = merge_query_endpoints(endpoints, &self.links, 30);
                self.log(LineKind::Phase, format!("활성 프로브 대상 {}개", endpoints.len()), None);
                self.queue = endpoints.into();
                self.phase = Phase::ProbeWorkers;
                // Hidden-path sweep, redteam only: one deterministic worker.
                if self.intensity == "full" {
                    let origin = url::Url::parse(&self.target)
                        .map(|parsed| parsed.origin().ascii_serialization())
                        .unwrap_or_else(|_| self.target.clone());
                    let prompt = path_probe_prompt(&origin, &site);
                    if let Err(error) =
                        Turn::spawn_into(&mut self.pool, spawn, &prompt, Some("scan".into()))
                    {
                        self.log(LineKind::Error, error, None);
                    }
                }
            }
            Phase::ProbeWorkers => {
                self.pump_pool(spawn, &mut actions);
                if self.pool.is_empty() && self.queue.is_empty() {
                    if self.intensity == "full" && self.aggressive {
                        self.log(LineKind::Phase, "에스컬레이션 — 임팩트 증명", None);
                        self.start_turn(spawn, &escalate_prompt());
                        self.phase = Phase::Escalate;
                    } else {
                        self.start_report(spawn);
                    }
                }
            }
            Phase::Escalate if turn_done => {
                self.take_turn_text();
                self.start_report(spawn);
            }
            Phase::Report if turn_done => {
                let turn = self.turn.take().expect("turn finished");
                let export = turn.outcome.export;
                let text = turn.outcome.final_text;
                self.report = export
                    .filter(|text| !text.trim().is_empty())
                    .unwrap_or_else(|| text.trim().to_owned());
                self.log(LineKind::Phase, format!("완료 — 발견 {}건", self.findings), None);
                self.phase = Phase::Done;
                actions.push(ScanAction::CloseScanTabs);
            }
            Phase::Failed(_) | Phase::Done => {}
            _ => {}
        }
        actions
    }

    /// Keep the pool at MAX_PARALLEL; reap finished workers.
    fn pump_pool(&mut self, spawn: &mut Spawner, _actions: &mut [ScanAction]) {
        let mut keep = Vec::new();
        for turn in std::mem::take(&mut self.pool) {
            if turn.done {
                if let Some(error) = turn.failed.clone() {
                    self.log(LineKind::Error, error, turn.label.clone());
                }
                self.findings += turn.outcome.finding_adds;
            } else {
                keep.push(turn);
            }
        }
        self.pool = keep;
        while self.pool.len() < MAX_PARALLEL {
            let Some(url) = self.queue.pop_front() else { break };
            let (prompt, label) = if self.phase == Phase::ProbeWorkers {
                let (method, rest) = url.split_once(' ').unwrap_or(("GET", url.as_str()));
                let checklist = if self.intensity == "full" {
                    checklist_for(method, rest)
                } else {
                    String::new()
                };
                let tail = if checklist.is_empty() {
                    String::new()
                } else {
                    format!("{checklist}\n")
                };
                (
                    format!(
                        "{PROBE_PROMPT}\n\n{}\n대상: {url}\n{tail}",
                        level_line(&self.intensity, self.aggressive)
                    ),
                    Some(method.to_owned()),
                )
            } else {
                (
                    worker_prompt(&url, self.intensity == "full", self.aggressive),
                    Some(short_tag(&url)),
                )
            };
            if let Err(error) = Turn::spawn_into(&mut self.pool, spawn, &prompt, label.clone()) {
                eprintln!("[scan] worker spawn failed: {error}");
                self.log(LineKind::Error, error, label);
            }
        }
    }

    fn start_discovery(&mut self, spawn: &mut Spawner) {
        let site = scope_key(&self.target).unwrap_or_default();
        let scope_host = url::Url::parse(&self.target)
            .map(|parsed| parsed.host_str().unwrap_or_default().to_owned())
            .unwrap_or_else(|_| self.target.clone());
        self.log(LineKind::Phase, format!("탐색 단계 — {site}"), None);
        self.start_turn(spawn, &discovery_prompt(&self.target, &scope_host, self.pages));
        self.phase = Phase::Discovery;
    }

    /// The target's origin (scheme://host[:port]).
    fn origin(&self) -> String {
        url::Url::parse(&self.target)
            .map(|parsed| parsed.origin().ascii_serialization())
            .unwrap_or_else(|_| self.target.clone())
    }

    fn enter_workers(&mut self, spawn: &mut Spawner, actions: &mut Vec<ScanAction>) {
        for url in self.links.clone() {
            actions.push(ScanAction::OpenTab(url.clone()));
            self.opened.push(url);
        }
        self.queue = self.links.clone().into();
        self.log(LineKind::Phase, format!("탭 {}개 병렬 점검", self.queue.len()), None);
        // Workers can start before the tabs attach; their first browser_tabs
        // call just won't see them yet — they retry by listing again.
        self.phase = Phase::Workers;
        self.pump_pool(spawn, actions);
    }

    fn start_report(&mut self, spawn: &mut Spawner) {
        self.log(LineKind::Phase, "보고서 작성", None);
        self.start_turn(spawn, &report_prompt(&self.target));
        self.phase = Phase::Report;
    }

    fn start_turn(&mut self, spawn: &mut Spawner, prompt: &str) {
        match spawn(prompt) {
            Ok(session) => {
                let mut turn = Turn {
                    session,
                    label: None,
                    outcome: TurnOutcome {
                        final_text: String::new(),
                        finding_adds: 0,
                        export: None,
                    },
                    failed: None,
                    done: false,
                };
                let _ = turn.session.send(prompt);
                self.turn = Some(turn);
            }
            Err(error) => {
                eprintln!("[scan] turn spawn failed: {error}");
                self.log(LineKind::Error, error, None);
                self.phase = Phase::Failed("세션을 시작하지 못했다".into());
            }
        }
    }

    fn take_turn_text(&mut self) -> String {
        self.turn
            .take()
            .map(|turn| {
                self.findings += turn.outcome.finding_adds;
                turn.outcome.final_text
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 같은_사이트_페이지_링크만_자산을_제외하고_뽑는다() {
        let site = scope_key("https://shop.example.test/a").unwrap();
        let text = r#"설명 ["https://shop.example.test/a","https://shop.example.test/b?x=1","https://other.test/c","https://shop.example.test/logo.png","https://shop.example.test/a"]"#;
        assert_eq!(
            parse_links(text, &site, 10),
            vec![
                "https://shop.example.test/a".to_owned(),
                "https://shop.example.test/b?x=1".to_owned()
            ]
        );
        assert_eq!(parse_links("배열 없음", &site, 10), Vec::<String>::new());
    }

    #[test]
    fn 엔드포인트는_메서드를_보존하고_경로로_중복을_뺀다() {
        let site = scope_key("https://shop.example.test/a").unwrap();
        let text = r#"["POST https://shop.example.test/api/x","GET https://shop.example.test/api/x?p=1","GET https://shop.example.test/api/x","https://other.test/y"]"#;
        // 같은 경로의 첫 등장(POST)만 남는다 — 쿼리 붙은 GET은 links 병합이 다시 싣는다.
        assert_eq!(
            parse_endpoints(text, &site, 10),
            vec!["POST https://shop.example.test/api/x".to_owned()]
        );
    }

    #[test]
    fn 쿼리를_가진_발견_링크가_같은_경로의_맨_get을_대체한다() {
        let endpoints = vec!["GET https://a.test/x".to_owned(), "POST https://a.test/y".to_owned()];
        let links = vec!["https://a.test/x?q=1".to_owned()];
        assert_eq!(
            merge_query_endpoints(endpoints, &links, 30),
            vec![
                "GET https://a.test/x?q=1".to_owned(),
                "POST https://a.test/y".to_owned()
            ]
        );
    }

    #[test]
    fn 연결된_호스트는_같은_사이트만_모은다() {
        let site = scope_key("https://app.test/").unwrap();
        let urls = vec![
            "https://api.app.test/v1/x".to_owned(),
            "https://cdn.other.com/lib.js".to_owned(),
            "https://app.test:8080/ws".to_owned(),
        ];
        assert_eq!(
            connected_hosts(&urls, &site),
            vec!["api.app.test".to_owned(), "app.test:8080".to_owned()]
        );
    }

    #[test]
    fn 체크리스트는_반사_가능성과_파라미터에_따라_고른다() {
        // No params, GET: only the auth replay.
        assert!(checklist_for("GET", "https://a.test/health").contains("broken_auth"));
        assert!(!checklist_for("GET", "https://a.test/health").contains("ssti"));
        // Query param: reflection suite + crlf; id value: idor.
        let list = checklist_for("GET", "https://a.test/i?uuid=00000000-0000-0000-0000-000000000000&q=x");
        for marker in ["broken_auth", "idor", "xss", "ssti", "cmdi", "sqli", "nosqli", "crlf"] {
            assert!(list.contains(marker), "{marker} missing:\n{list}");
        }
        // URL param: ssrf.
        assert!(checklist_for("GET", "https://a.test/r?redirect=x").contains("ssrf"));
        // POST body: reflection even without query.
        assert!(checklist_for("POST", "https://a.test/form").contains("ssti"));
    }

    #[test]
    fn 로그_태그는_마지막_경로_조각으로_짧게_준다() {
        assert_eq!(short_tag("https://a.test/api/v3/items?page=1"), "items");
        assert_eq!(short_tag("https://a.test/"), "a.test");
        assert_eq!(short_tag("https://a.test/경로이름이열여덟글자정도는됩니다"), "경로이름이열여덟글자정도는됩니…");
    }

    #[test]
    fn 강도_문구는_모드와_일회성_여부를_따른다() {
        assert!(level_line("probe", true).contains("payload_probe/path_probe/crlf_test 금지"));
        assert!(level_line("full", true).contains("적응 재시도"));
        assert!(level_line("full", false).contains("변형 요청 최대 1개"));
    }
}
