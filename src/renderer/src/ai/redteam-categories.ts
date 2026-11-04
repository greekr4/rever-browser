// Category-driven vulnerability pass (OWASP API Top 10 / WSTG), the active
// counterpart to the hidden-path Scanner. The STORE classifies each in-scope
// endpoint here and feeds the worker an explicit per-endpoint checklist, so
// coverage is deterministic instead of left to the agent's discretion.
//
// Only categories with a backing single-shot rever tool are listed — no faked
// coverage. Each check is one or two requests; loops/fuzzing stay forbidden.

export type VulnId =
  | 'broken_auth'
  | 'idor'
  | 'xss'
  | 'ssti'
  | 'cmdi'
  | 'sqli'
  | 'nosqli'
  | 'pathwrite'
  | 'crlf'
  | 'ssrf'
  | 'lfi'

export interface VulnCategory {
  id: VulnId
  owasp: string
  severity: 'low' | 'medium' | 'high' | 'critical'
  /** One imperative line telling the worker exactly which tool to run. */
  check: string
}

// URL-ish parameter names that commonly fetch/redirect to a server-side URL.
const URL_PARAM_RE = /^(url|uri|redirect|redirect_uri|return|return_url|next|callback|dest|destination|target|continue|feed|image|img|webhook|link|site|domain|host|path|file|page|load|src)$/i
// Parameter names / path hints that read a file (LFI / traversal candidates).
const FILE_PARAM_RE = /^(file|filename|path|template|page|doc|document|include|view|download|read|load)$/i
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i

function parse(url: string): { params: string[]; segs: string[] } | null {
  try {
    const u = new URL(url)
    return {
      params: [...u.searchParams.keys()],
      segs: u.pathname.split('/').filter(Boolean)
    }
  } catch {
    return null
  }
}

function hasIdParam(url: string): boolean {
  const p = parse(url)
  if (!p) return false
  // id-like path segment (numeric or uuid) or a query key/value that looks like an id
  if (p.segs.some((s) => /^\d+$/.test(s) || UUID_RE.test(s))) return true
  try {
    const u = new URL(url)
    for (const [k, v] of u.searchParams) {
      if (/(^|_)(id|uid|uuid|user|account|order|pid|no|seq)$/i.test(k)) return true
      if (/^\d+$/.test(v) || UUID_RE.test(v)) return true
    }
  } catch {
    return false
  }
  return false
}

const CHECKS: Record<VulnId, Omit<VulnCategory, 'id'>> = {
  broken_auth: {
    owasp: 'API2:Broken Authentication',
    severity: 'high',
    check:
      'noauth_probe({ requestId }) 를 1회 실행한다 — 캡처 요청을 raw 소켓으로 (1) 오너 세션(파티션에서 자동) (2) 쿠키 없이 두 번 보내, 쿠키 없이도 같은 보호 리소스가 오면 인증 우회 확정(high). replay_request 로 Cookie 를 비워도 httpOnly 세션은 credentials:include 로 재첨부되어 "무인증 200" 오탐이 나므로, 반드시 noauth_probe 를 쓴다. loopback/lab 게이트. noauth_probe 가 불가한(비-loopback) 경우에만 replay_request 로 약식 확인하되 확정하지 말 것.'
  },
  idor: {
    owasp: 'API1:BOLA/IDOR',
    severity: 'high',
    check:
      '객체 id(경로 UUID/숫자 또는 쿼리 id)로 접근하는 인증 리소스면 idor_probe({ requestId }) 를 1회 실행한다 — 대체 테넌트 세션(auth 부트스트랩이 redteam_alt_session_set 으로 저장)으로 소유자 요청을 재생해, 익명은 거부되는데 다른 테넌트가 같은 리소스를 읽으면 BOLA/IDOR 확정(교차 테넌트 데이터 접근, high/critical). 대체 세션이 없으면(단일 신원) 숫자 id ±1 또는 다른 uuid 로 replay_request 1회로 약식 확인하되, 확정은 idor_probe 로만. 익명도 같은 리소스를 주면 그건 BOLA 가 아니라 공개 노출(broken auth)이니 그렇게 기록.'
  },
  xss: {
    owasp: 'WSTG-INPV:Reflected XSS',
    severity: 'medium',
    check:
      '반사되는 파라미터(쿼리 또는 폼 바디)를 § 로 표시하고 payload_probe({ requestId }) 로 반사형 XSS 를 확인한다. 응답에 페이로드가 이스케이프 없이 반사되면 보고.'
  },
  ssti: {
    owasp: 'WSTG-INPV:Server-Side Template Injection',
    severity: 'high',
    check:
      'ssti_probe({ requestId, param }) 를 1회 실행한다(§ 마커 스테이징 불필요) — Velocity/Freemarker/Jinja/ERB/Thymeleaf/Razor 를 한 번에 시험한다. 한 엔진이라도 산술을 평가하면(리터럴이 아닌 계산 결과가 응답에 나타나면) SSTI 확정: RCE 급이므로 severity:"critical" 로 올려 보고한다. {{7*7}}/${7*7} 하나가 리터럴로 반사된다고 SSTI 를 기각하지 말 것 — Java 스택은 ${…}/#set 으로만 평가된다.'
  },
  cmdi: {
    owasp: 'WSTG-INPV:OS Command Injection',
    severity: 'critical',
    check:
      'cmd_probe({ requestId, param }) 를 1회 실행한다 — quote break/;/|/backtick/$() 컨텍스트를 한 번에 시험한다. 셸이 nonce 산술을 계산해 돌려보내면(리터럴이 아닌 계산 결과) 커맨드 인젝션 확정: RCE 급. date/format/exec 류 파라미터가 전형적 매개체이며, 페이로드가 리터럴로 반사되거나 인용부호가 깨진 에러가 나도 기각하지 말 것 — 도구의 산술 결과만이 판정 근거다.'
  },
  sqli: {
    owasp: 'WSTG-INPV:SQL Injection',
    severity: 'critical',
    check:
      'sqli_probe({ requestId, param }) 를 1회 실행한다 — quote 에러 시그니처와 boolean 산술(true/false) 차등을 한 번에 시험한다. SQL 에러 문자열이 응답에 드러나거나 true/false 페이로드의 응답이 차등을 보이면 SQL 인젝션 확정: 데이터 유출 가능 critical. 로그인/검색 폼 필드(username·q)가 전형적 매개체이며, 페이로드가 리터럴로 반사되어도 기각하지 말 것 — 도구의 에러 시그니처·차등만이 판정 근거다.'
  },
  nosqli: {
    owasp: 'WSTG-INPV:NoSQL / Cypher Injection',
    severity: 'critical',
    check:
      'nosqli_probe({ requestId, param }) 를 1회 실행한다 — 백엔드가 Mongo/Neo4j 로 보이면(세션 쿠키·스택·드라이버 흔적) sqli_probe 대신 이것을 쓴다(sqli_probe 는 SQL 에러 기반이라 NoSQL 에 장님). param[$ne] vs param[$eq] 연산자 차등과 Mongo/Neo4j 파서 에러 시그니처를 한 번에 시험한다. 응답이 차등을 보이거나 드라이버 에러가 드러나면 NoSQL/Cypher 인젝션 확정: 인증 우회·데이터 유출 가능 critical. 로그인/검색 폼(username·q)이 전형적 매개체이며, bcrypt 로 막힌 로그인은 password 가 쿼리 밖에서 비교되어 음성이 정상이다.'
  },
  pathwrite: {
    owasp: 'WSTG-ATHZ:Path Traversal / Arbitrary File Write',
    severity: 'critical',
    check:
      '저장-후-표시형 폼(테스티모니얼/댓글/업로드 등)에서 값이 서버 파일명이 될 만한 필드(name/customer/filename/title 류)를 param 으로 지정해 path_write_probe({ requestId, param }) 를 1회 실행한다. 평문 이름 마커는 되읽기 페이지에 뜨는데 ../ 이름 마커는 안 뜨면 의도 디렉토리 밖 임의 파일 쓰기 확정: 소스/템플릿(.templ/.html/.php) 덮어쓰기로 RCE 가능 — critical. 반사 프로브가 "저장되지만 이스케이프됨(XSS/SSTI 아님)"이라고 할 때 그 폼의 파일명 필드가 진짜 싱크다. 되읽기 URL 이 다르면 readback 으로 지정한다.'
  },
  crlf: {
    owasp: 'WSTG-INPV:CRLF/Header Injection',
    severity: 'medium',
    check: 'crlf_test({ requestId }) 로 헤더 주입을 1회 확인한다.'
  },
  ssrf: {
    owasp: 'API7:SSRF / Open Redirect',
    severity: 'high',
    check:
      'URL 류 파라미터에 내부 주소(http://169.254.169.254/ 또는 http://localhost/)를 넣어 replay_request 로 1회 재생한다. 서버가 그 주소를 가져오거나 리다이렉트하면 SSRF/오픈리다이렉트.'
  },
  lfi: {
    owasp: 'WSTG-ATHZ:LFI/Path Traversal',
    severity: 'high',
    check: '파일/경로 파라미터가 § 마커로 지정되면 lfi_probe 로 경로 traversal 을 1회 확인한다.'
  }
}

// Which active categories apply to one endpoint, by cheap URL heuristics.
export function classify(method: string, url: string): VulnCategory[] {
  const p = parse(url)
  if (!p) return []
  const ids: VulnId[] = ['broken_auth'] // every captured request can be auth-replayed
  if (hasIdParam(url)) ids.push('idor')
  const params = p.params
  // Reflected-input sinks (XSS, template injection) take user input from a query
  // param OR a request body, so they apply to GET-with-query and to any
  // body-bearing method — a POST form field is the classic SSTI surface.
  const bodyMethod = ['POST', 'PUT', 'PATCH'].includes(method.toUpperCase())
  const reflectable = params.length > 0 || bodyMethod
  if (reflectable) {
    ids.push('xss')
    ids.push('ssti')
    ids.push('cmdi')
    ids.push('sqli')
    ids.push('nosqli')
  }
  // Stored-then-displayed forms (a filename field + a content field, or any
  // body-bearing write) can hide an arbitrary-file-write sink the reflection
  // probes miss — only worth it when there's more than one field to carry
  // filename + content.
  if (reflectable && (params.length >= 2 || bodyMethod)) ids.push('pathwrite')
  if (params.length) ids.push('crlf')
  if (params.some((k) => URL_PARAM_RE.test(k))) ids.push('ssrf')
  if (params.some((k) => FILE_PARAM_RE.test(k)) || p.segs.some((s) => FILE_PARAM_RE.test(s))) {
    ids.push('lfi')
  }
  return ids.map((id) => ({ id, ...CHECKS[id] }))
}

// The per-endpoint checklist snippet appended to the worker prompt. Empty when
// nothing applies (then only the base single-shot rules run).
export function checklistFor(method: string, url: string): string {
  const cats = classify(method, url)
  if (!cats.length) return ''
  const lines = cats.map(
    (c) => `- [${c.owasp}] ${c.check} 발견 시 finding_add(category:"vuln", severity:"${c.severity}", tags:["${c.id}"]).`
  )
  return `적용 취약점 카테고리(해당되는 것만, 각 단발):\n${lines.join('\n')}`
}
