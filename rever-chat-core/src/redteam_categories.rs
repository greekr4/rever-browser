//! Port of `src/renderer/src/ai/redteam-categories.ts` — the category-driven vulnerability pass.
//!
//! Category-driven vulnerability pass (OWASP API Top 10 / WSTG), the active
//! counterpart to the hidden-path Scanner. The STORE classifies each in-scope
//! endpoint here and feeds the worker an explicit per-endpoint checklist, so
//! coverage is deterministic instead of left to the agent's discretion.
//!
//! Only categories with a backing single-shot rever tool are listed — no faked
//! coverage. Each check is one or two requests; loops/fuzzing stay forbidden.

use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VulnId {
    BrokenAuth,
    Idor,
    Xss,
    Ssti,
    Cmdi,
    Sqli,
    Nosqli,
    Pathwrite,
    Crlf,
    Ssrf,
    Lfi,
}

impl VulnId {
    /// The id as the TypeScript spelled it, which is also the finding tag.
    pub fn as_str(self) -> &'static str {
        match self {
            VulnId::BrokenAuth => "broken_auth",
            VulnId::Idor => "idor",
            VulnId::Xss => "xss",
            VulnId::Ssti => "ssti",
            VulnId::Cmdi => "cmdi",
            VulnId::Sqli => "sqli",
            VulnId::Nosqli => "nosqli",
            VulnId::Pathwrite => "pathwrite",
            VulnId::Crlf => "crlf",
            VulnId::Ssrf => "ssrf",
            VulnId::Lfi => "lfi",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VulnCategory {
    pub id: VulnId,
    pub owasp: &'static str,
    pub severity: Severity,
    /// One imperative line telling the worker exactly which tool to run.
    pub check: &'static str,
}

/// URL-ish parameter names that commonly fetch/redirect to a server-side URL.
const URL_PARAMS: &[&str] = &[
    "url",
    "uri",
    "redirect",
    "redirect_uri",
    "return",
    "return_url",
    "next",
    "callback",
    "dest",
    "destination",
    "target",
    "continue",
    "feed",
    "image",
    "img",
    "webhook",
    "link",
    "site",
    "domain",
    "host",
    "path",
    "file",
    "page",
    "load",
    "src",
];
/// Parameter names / path hints that read a file (LFI / traversal candidates).
const FILE_PARAMS: &[&str] = &[
    "file", "filename", "path", "template", "page", "doc", "document", "include", "view",
    "download", "read", "load",
];
/// `/(^|_)(id|uid|uuid|user|account|order|pid|no|seq)$/i`
const ID_KEY_SUFFIXES: &[&str] = &[
    "id", "uid", "uuid", "user", "account", "order", "pid", "no", "seq",
];

/// `/^(a|b|…)$/i` — without the `u` flag the `i` flag folds ASCII letters only.
fn is_one_of(names: &[&str], s: &str) -> bool {
    names.iter().any(|name| name.eq_ignore_ascii_case(s))
}

/// `/^\d+$/` — JavaScript's `\d` is ASCII digits only.
fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i`
fn is_uuid(s: &str) -> bool {
    let mut groups = 0;
    s.split('-').zip([8, 4, 4, 4, 12]).all(|(group, len)| {
        groups += 1;
        group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit())
    }) && groups == 5
        && s.split('-').count() == 5
}

fn is_id_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ID_KEY_SUFFIXES.iter().any(|suffix| {
        key.strip_suffix(suffix)
            .is_some_and(|before| before.is_empty() || before.ends_with('_'))
    })
}

struct Parsed {
    /// Decoded query pairs, in order (`URLSearchParams`).
    pairs: Vec<(String, String)>,
    /// Non-empty path segments, still percent-encoded (`pathname`).
    segs: Vec<String>,
}

fn parse(url: &str) -> Option<Parsed> {
    let u = Url::parse(url).ok()?;
    Some(Parsed {
        pairs: u.query_pairs().into_owned().collect(),
        segs: u
            .path()
            .split('/')
            .filter(|seg| !seg.is_empty())
            .map(str::to_string)
            .collect(),
    })
}

fn has_id_param(p: &Parsed) -> bool {
    // id-like path segment (numeric or uuid) or a query key/value that looks like an id
    p.segs.iter().any(|s| is_digits(s) || is_uuid(s))
        || p.pairs
            .iter()
            .any(|(k, v)| is_id_key(k) || is_digits(v) || is_uuid(v))
}

fn check_of(id: VulnId) -> (&'static str, Severity, &'static str) {
    match id {
        VulnId::BrokenAuth => (
            "API2:Broken Authentication",
            Severity::High,
            r#"noauth_probe({ requestId }) 를 1회 실행한다 — 캡처 요청을 raw 소켓으로 (1) 오너 세션(파티션에서 자동) (2) 쿠키 없이 두 번 보내, 쿠키 없이도 같은 보호 리소스가 오면 인증 우회 확정(high). replay_request 로 Cookie 를 비워도 httpOnly 세션은 credentials:include 로 재첨부되어 "무인증 200" 오탐이 나므로, 반드시 noauth_probe 를 쓴다. loopback/lab 게이트. noauth_probe 가 불가한(비-loopback) 경우에만 replay_request 로 약식 확인하되 확정하지 말 것."#,
        ),
        VulnId::Idor => (
            "API1:BOLA/IDOR",
            Severity::High,
            r#"객체 id(경로 UUID/숫자 또는 쿼리 id)로 접근하는 인증 리소스면 idor_probe({ requestId }) 를 1회 실행한다 — 대체 테넌트 세션(auth 부트스트랩이 redteam_alt_session_set 으로 저장)으로 소유자 요청을 재생해, 익명은 거부되는데 다른 테넌트가 같은 리소스를 읽으면 BOLA/IDOR 확정(교차 테넌트 데이터 접근, high/critical). 대체 세션이 없으면(단일 신원) 숫자 id ±1 또는 다른 uuid 로 replay_request 1회로 약식 확인하되, 확정은 idor_probe 로만. 익명도 같은 리소스를 주면 그건 BOLA 가 아니라 공개 노출(broken auth)이니 그렇게 기록."#,
        ),
        VulnId::Xss => (
            "WSTG-INPV:Reflected XSS",
            Severity::Medium,
            r#"반사되는 파라미터(쿼리 또는 폼 바디)를 § 로 표시하고 payload_probe({ requestId }) 로 반사형 XSS 를 확인한다. 응답에 페이로드가 이스케이프 없이 반사되면 보고."#,
        ),
        VulnId::Ssti => (
            "WSTG-INPV:Server-Side Template Injection",
            Severity::High,
            r#"ssti_probe({ requestId, param }) 를 1회 실행한다(§ 마커 스테이징 불필요) — Velocity/Freemarker/Jinja/ERB/Thymeleaf/Razor 를 한 번에 시험한다. 한 엔진이라도 산술을 평가하면(리터럴이 아닌 계산 결과가 응답에 나타나면) SSTI 확정: RCE 급이므로 severity:"critical" 로 올려 보고한다. {{7*7}}/${7*7} 하나가 리터럴로 반사된다고 SSTI 를 기각하지 말 것 — Java 스택은 ${…}/#set 으로만 평가된다."#,
        ),
        VulnId::Cmdi => (
            "WSTG-INPV:OS Command Injection",
            Severity::Critical,
            r#"cmd_probe({ requestId, param }) 를 1회 실행한다 — quote break/;/|/backtick/$() 컨텍스트를 한 번에 시험한다. 셸이 nonce 산술을 계산해 돌려보내면(리터럴이 아닌 계산 결과) 커맨드 인젝션 확정: RCE 급. date/format/exec 류 파라미터가 전형적 매개체이며, 페이로드가 리터럴로 반사되거나 인용부호가 깨진 에러가 나도 기각하지 말 것 — 도구의 산술 결과만이 판정 근거다."#,
        ),
        VulnId::Sqli => (
            "WSTG-INPV:SQL Injection",
            Severity::Critical,
            r#"sqli_probe({ requestId, param }) 를 1회 실행한다 — quote 에러 시그니처와 boolean 산술(true/false) 차등을 한 번에 시험한다. SQL 에러 문자열이 응답에 드러나거나 true/false 페이로드의 응답이 차등을 보이면 SQL 인젝션 확정: 데이터 유출 가능 critical. 로그인/검색 폼 필드(username·q)가 전형적 매개체이며, 페이로드가 리터럴로 반사되어도 기각하지 말 것 — 도구의 에러 시그니처·차등만이 판정 근거다."#,
        ),
        VulnId::Nosqli => (
            "WSTG-INPV:NoSQL / Cypher Injection",
            Severity::Critical,
            r#"nosqli_probe({ requestId, param }) 를 1회 실행한다 — 백엔드가 Mongo/Neo4j 로 보이면(세션 쿠키·스택·드라이버 흔적) sqli_probe 대신 이것을 쓴다(sqli_probe 는 SQL 에러 기반이라 NoSQL 에 장님). param[$ne] vs param[$eq] 연산자 차등과 Mongo/Neo4j 파서 에러 시그니처를 한 번에 시험한다. 응답이 차등을 보이거나 드라이버 에러가 드러나면 NoSQL/Cypher 인젝션 확정: 인증 우회·데이터 유출 가능 critical. 로그인/검색 폼(username·q)이 전형적 매개체이며, bcrypt 로 막힌 로그인은 password 가 쿼리 밖에서 비교되어 음성이 정상이다."#,
        ),
        VulnId::Pathwrite => (
            "WSTG-ATHZ:Path Traversal / Arbitrary File Write",
            Severity::Critical,
            r#"저장-후-표시형 폼(테스티모니얼/댓글/업로드 등)에서 값이 서버 파일명이 될 만한 필드(name/customer/filename/title 류)를 param 으로 지정해 path_write_probe({ requestId, param }) 를 1회 실행한다. 평문 이름 마커는 되읽기 페이지에 뜨는데 ../ 이름 마커는 안 뜨면 의도 디렉토리 밖 임의 파일 쓰기 확정: 소스/템플릿(.templ/.html/.php) 덮어쓰기로 RCE 가능 — critical. 반사 프로브가 "저장되지만 이스케이프됨(XSS/SSTI 아님)"이라고 할 때 그 폼의 파일명 필드가 진짜 싱크다. 되읽기 URL 이 다르면 readback 으로 지정한다."#,
        ),
        VulnId::Crlf => (
            "WSTG-INPV:CRLF/Header Injection",
            Severity::Medium,
            r#"crlf_test({ requestId }) 로 헤더 주입을 1회 확인한다."#,
        ),
        VulnId::Ssrf => (
            "API7:SSRF / Open Redirect",
            Severity::High,
            r#"URL 류 파라미터에 내부 주소(http://169.254.169.254/ 또는 http://localhost/)를 넣어 replay_request 로 1회 재생한다. 서버가 그 주소를 가져오거나 리다이렉트하면 SSRF/오픈리다이렉트."#,
        ),
        VulnId::Lfi => (
            "WSTG-ATHZ:LFI/Path Traversal",
            Severity::High,
            r#"파일/경로 파라미터가 § 마커로 지정되면 lfi_probe 로 경로 traversal 을 1회 확인한다."#,
        ),
    }
}

fn category(id: VulnId) -> VulnCategory {
    let (owasp, severity, check) = check_of(id);
    VulnCategory {
        id,
        owasp,
        severity,
        check,
    }
}

/// Which active categories apply to one endpoint, by cheap URL heuristics.
pub fn classify(method: &str, url: &str) -> Vec<VulnCategory> {
    let Some(p) = parse(url) else {
        return Vec::new();
    };
    let mut ids = vec![VulnId::BrokenAuth]; // every captured request can be auth-replayed
    if has_id_param(&p) {
        ids.push(VulnId::Idor);
    }
    let params: Vec<&str> = p.pairs.iter().map(|(k, _)| k.as_str()).collect();
    // Reflected-input sinks (XSS, template injection) take user input from a query
    // param OR a request body, so they apply to GET-with-query and to any
    // body-bearing method — a POST form field is the classic SSTI surface.
    let body_method = matches!(method.to_uppercase().as_str(), "POST" | "PUT" | "PATCH");
    let reflectable = !params.is_empty() || body_method;
    if reflectable {
        ids.extend([
            VulnId::Xss,
            VulnId::Ssti,
            VulnId::Cmdi,
            VulnId::Sqli,
            VulnId::Nosqli,
        ]);
    }
    // Stored-then-displayed forms (a filename field + a content field, or any
    // body-bearing write) can hide an arbitrary-file-write sink the reflection
    // probes miss — only worth it when there's more than one field to carry
    // filename + content.
    if reflectable && (params.len() >= 2 || body_method) {
        ids.push(VulnId::Pathwrite);
    }
    if !params.is_empty() {
        ids.push(VulnId::Crlf);
    }
    if params.iter().any(|k| is_one_of(URL_PARAMS, k)) {
        ids.push(VulnId::Ssrf);
    }
    if params.iter().any(|k| is_one_of(FILE_PARAMS, k))
        || p.segs.iter().any(|s| is_one_of(FILE_PARAMS, s))
    {
        ids.push(VulnId::Lfi);
    }
    ids.into_iter().map(category).collect()
}

/// The per-endpoint checklist snippet appended to the worker prompt. Empty when
/// nothing applies (then only the base single-shot rules run).
pub fn checklist_for(method: &str, url: &str) -> String {
    let cats = classify(method, url);
    if cats.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = cats
        .iter()
        .map(|c| {
            format!(
                "- [{}] {} 발견 시 finding_add(category:\"vuln\", severity:\"{}\", tags:[\"{}\"]).",
                c.owasp,
                c.check,
                c.severity.as_str(),
                c.id.as_str()
            )
        })
        .collect();
    format!(
        "적용 취약점 카테고리(해당되는 것만, 각 단발):\n{}",
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(method: &str, url: &str) -> Vec<VulnId> {
        classify(method, url).iter().map(|c| c.id).collect()
    }

    #[test]
    fn 파라미터_없는_get은_인증_재생만_적용된다() {
        assert_eq!(ids("GET", "https://ex.com/health"), [VulnId::BrokenAuth]);
    }

    #[test]
    fn 경로에_숫자_id가_있으면_idor를_적용한다() {
        assert!(ids("GET", "https://ex.com/users/42").contains(&VulnId::Idor));
    }

    #[test]
    fn uuid_경로_리소스는_idor_probe_체크라인을_받는다() {
        let s = checklist_for(
            "GET",
            "https://ex.com/v1/ats/jobs/cdb11ac2-9993-4ac6-bb9d-6959df5228a9",
        );
        assert!(s.contains("idor_probe"));
        assert!(s.contains("API1:BOLA/IDOR"));
    }

    #[test]
    fn broken_auth_체크는_noauth_probe를_쓴다() {
        let s = checklist_for("GET", "https://ex.com/health");
        assert!(s.contains("noauth_probe"));
        assert!(s.contains("API2:Broken Authentication"));
    }

    #[test]
    fn uuid_경로도_idor로_본다() {
        assert!(
            ids(
                "GET",
                "https://ex.com/orders/550e8400-e29b-41d4-a716-446655440000"
            )
            .contains(&VulnId::Idor)
        );
    }

    #[test]
    fn get_쿼리_파라미터가_있으면_반사_체크와_crlf를_적용하고_id류가_아니면_idor는_아니다() {
        let c = ids("GET", "https://ex.com/search?q=hi");
        assert!(c.contains(&VulnId::Xss));
        assert!(c.contains(&VulnId::Ssti));
        assert!(c.contains(&VulnId::Cmdi));
        assert!(c.contains(&VulnId::Sqli));
        assert!(c.contains(&VulnId::Crlf));
        assert!(!c.contains(&VulnId::Idor));
    }

    #[test]
    fn date_format류_쿼리_파라미터는_cmdi_체크라인을_받는다_timekorp_회귀() {
        let s = checklist_for("GET", "http://localhost:1337/?format=%Y");
        assert!(s.contains("cmd_probe"));
        assert!(s.contains("WSTG-INPV:OS Command Injection"));
        assert!(s.contains("severity:\"critical\""));
    }

    #[test]
    fn 로그인_폼_post는_sqli_체크라인을_받는다_korp_terminal_회귀() {
        let s = checklist_for("POST", "http://localhost:1337/");
        assert!(s.contains("sqli_probe"));
        assert!(s.contains("WSTG-INPV:SQL Injection"));
        assert!(s.contains("severity:\"critical\""));
    }

    #[test]
    fn 로그인_폼_post는_nosql_체크라인도_받는다_mongo_neo4j_스택() {
        let s = checklist_for("POST", "http://localhost:1337/");
        assert!(s.contains("nosqli_probe"));
        assert!(s.contains("WSTG-INPV:NoSQL / Cypher Injection"));
        assert!(s.contains("severity:\"critical\""));
    }

    #[test]
    fn 반사_가능_엔드포인트는_sqli와_nosqli를_함께_적용한다() {
        let c = ids("GET", "https://ex.com/search?q=hi");
        assert!(c.contains(&VulnId::Sqli));
        assert!(c.contains(&VulnId::Nosqli));
    }

    #[test]
    fn 다중_필드_저장_폼은_path_write_probe_라인을_받는다_testimonial_회귀() {
        let s = checklist_for("GET", "http://localhost:1337/?customer=a&testimonial=b");
        assert!(s.contains("path_write_probe"));
        assert!(s.contains("WSTG-ATHZ:Path Traversal / Arbitrary File Write"));
    }

    #[test]
    fn 단일_쿼리_파라미터는_path_write_probe를_받지_않는다_과발사_방지() {
        assert!(!ids("GET", "https://ex.com/search?q=hi").contains(&VulnId::Pathwrite));
    }

    #[test]
    fn post는_바디가_반사_싱크이므로_xss_ssti를_적용한다_쿼리_없어도() {
        let c = ids("POST", "https://ex.com/");
        assert!(c.contains(&VulnId::Xss));
        assert!(c.contains(&VulnId::Ssti));
    }

    #[test]
    fn 파라미터_없는_get은_반사_체크를_적용하지_않는다() {
        let c = ids("GET", "https://ex.com/health");
        assert!(!c.contains(&VulnId::Xss));
        assert!(!c.contains(&VulnId::Ssti));
    }

    #[test]
    fn url류_파라미터가_있으면_ssrf를_적용한다() {
        assert!(ids("GET", "https://ex.com/fetch?redirect=https://x").contains(&VulnId::Ssrf));
        assert!(ids("GET", "https://ex.com/img?image=a.png").contains(&VulnId::Ssrf));
    }

    #[test]
    fn 파일_경로_파라미터가_있으면_lfi를_적용한다() {
        assert!(ids("GET", "https://ex.com/view?file=report.pdf").contains(&VulnId::Lfi));
    }

    #[test]
    fn 파싱_불가_url은_빈_배열() {
        assert_eq!(classify("GET", "not a url"), []);
    }

    #[test]
    fn 적용_카테고리를_owasp_라벨_태그와_함께_나열한다() {
        let s = checklist_for("GET", "https://ex.com/users/42?redirect=x");
        assert!(s.contains("API1:BOLA/IDOR"));
        assert!(s.contains("API7:SSRF"));
        assert!(s.contains("tags:[\"idor\"]"));
    }

    #[test]
    fn 해당_없으면_빈_문자열() {
        assert_eq!(checklist_for("GET", "not a url"), "");
    }

    // The cases below are not in the TypeScript test file.

    #[test]
    fn 카테고리_순서와_체크리스트_줄_형식이_고정이다() {
        assert_eq!(
            ids("post", "https://ex.com/users/42?file=a&next=b"),
            [
                VulnId::BrokenAuth,
                VulnId::Idor,
                VulnId::Xss,
                VulnId::Ssti,
                VulnId::Cmdi,
                VulnId::Sqli,
                VulnId::Nosqli,
                VulnId::Pathwrite,
                VulnId::Crlf,
                VulnId::Ssrf,
                VulnId::Lfi,
            ]
        );
        assert_eq!(
            checklist_for("GET", "https://ex.com/a?x=y")
                .lines()
                .last()
                .unwrap_or_default(),
            "- [WSTG-INPV:CRLF/Header Injection] crlf_test({ requestId }) 로 헤더 주입을 1회 확인한다. 발견 시 finding_add(category:\"vuln\", severity:\"medium\", tags:[\"crlf\"])."
        );
    }

    #[test]
    fn id_판정은_키_접미사와_값_모양을_본다() {
        assert!(ids("GET", "https://ex.com/a?USER_ID=x").contains(&VulnId::Idor));
        assert!(ids("GET", "https://ex.com/a?uid=x").contains(&VulnId::Idor));
        assert!(ids("GET", "https://ex.com/a?q=123").contains(&VulnId::Idor));
        // "void" ends in "id" but not after "_" or at the start.
        assert!(!ids("GET", "https://ex.com/a?void=x").contains(&VulnId::Idor));
        // Fullwidth digits are not \d in JavaScript.
        assert!(!ids("GET", "https://ex.com/a?q=%EF%BC%91").contains(&VulnId::Idor));
        assert!(
            !ids(
                "GET",
                "https://ex.com/o/550e8400-e29b-41d4-a716-446655440000-1"
            )
            .contains(&VulnId::Idor)
        );
    }

    #[test]
    fn 경로_세그먼트의_파일_힌트도_lfi로_보고_직렬화는_ts_필드명을_쓴다() {
        assert!(ids("GET", "https://ex.com/api/Download").contains(&VulnId::Lfi));
        assert!(!ids("GET", "https://ex.com/api/downloads").contains(&VulnId::Lfi));
        let first = classify("GET", "https://ex.com/health").remove(0);
        let json = serde_json::to_value(&first).unwrap_or_default();
        assert_eq!(json["id"], "broken_auth");
        assert_eq!(json["owasp"], "API2:Broken Authentication");
        assert_eq!(json["severity"], "high");
        assert!(json["check"].is_string());
    }

    // Checked against the TypeScript with bun: URLSearchParams decoding decides
    // what counts as a key, a value and a second field.
    #[test]
    fn 쿼리_디코딩과_메서드_대문자화를_타입스크립트와_같게_본다() {
        let reflect = [
            VulnId::Xss,
            VulnId::Ssti,
            VulnId::Cmdi,
            VulnId::Sqli,
            VulnId::Nosqli,
        ];
        let with = |head: &[VulnId], tail: &[VulnId]| [head, &reflect, tail].concat();
        let (auth, idor, crlf) = (VulnId::BrokenAuth, VulnId::Idor, VulnId::Crlf);

        // A repeated key is two fields; a key without "=" is still a parameter.
        assert_eq!(
            ids("GET", "https://ex.com/search?q=1x&q=2x"),
            with(&[auth], &[VulnId::Pathwrite, crlf])
        );
        assert_eq!(
            ids("GET", "https://ex.com/search?q"),
            with(&[auth], &[crlf])
        );
        assert_eq!(ids("GET", "https://ex.com/a?&&"), [auth]);
        assert_eq!(ids("GET", "https://ex.com/a#?id=1"), [auth]);
        // Keys and values are percent-decoded before the id tests; "+" is a space.
        assert_eq!(
            ids("GET", "https://ex.com/a?i%64=x"),
            with(&[auth, idor], &[crlf])
        );
        assert_eq!(
            ids("GET", "https://ex.com/a?q=12%33"),
            with(&[auth, idor], &[crlf])
        );
        assert_eq!(
            ids("GET", "https://ex.com/a?q=+123"),
            with(&[auth], &[crlf])
        );
        assert_eq!(
            ids("GET", "https://ex.com/a?id%0A=x"),
            with(&[auth], &[crlf])
        );
        // Path segments stay percent-encoded, and empty ones are dropped.
        assert_eq!(ids("GET", "https://ex.com/%66ile"), [auth]);
        assert_eq!(ids("GET", "https://ex.com//users//042/"), [auth, idor]);
        // toUpperCase is Unicode-aware ("ſ" becomes "S") but does not trim.
        assert_eq!(
            ids("poſt", "https://ex.com/"),
            with(&[auth], &[VulnId::Pathwrite])
        );
        assert_eq!(ids("PATCH ", "https://ex.com/"), [auth]);
        assert_eq!(ids("DELETE", "https://ex.com/"), [auth]);
    }
}
