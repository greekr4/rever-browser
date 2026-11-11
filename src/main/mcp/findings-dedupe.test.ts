import { describe, expect, it } from 'vitest'

import {
  endpointKeyOf,
  mergeDuplicateEndpoints,
  vulnKeyOf,
  isScanNoise,
  type DedupableFinding
} from './findings-dedupe'

function f(over: Partial<DedupableFinding> & Pick<DedupableFinding, 'title' | 'body'>): DedupableFinding {
  return {
    id: Math.random().toString(36).slice(2),
    category: 'endpoint',
    severity: 'low',
    status: 'candidate',
    tags: [],
    evidenceIds: [],
    createdAt: Date.now(),
    ...over
  }
}

describe('endpointKeyOf', () => {
  it('같은 host+path 는 같은 키를 낸다 (쿼리/프래그먼트 제거)', () => {
    const a = endpointKeyOf(f({ title: '댓글 API', body: 'GET https://site.com/api/comments?slug=x' }))
    const b = endpointKeyOf(f({ title: 'site.com — comments API (/api/comments)', body: '다른 관측' }))
    expect(a).toBe('site.com/api/comments')
    expect(b).toBe('site.com/api/comments')
  })

  it('URL 이 없으면 첫 절대경로를 키로 쓴다', () => {
    const k = endpointKeyOf(f({ title: 'calc — 비인증 API (/api/comments)', body: '관측만' }))
    expect(k).toBe('/api/comments')
  })

  it('같은 host 라도 포트가 다르면 다른 사이트로 본다', () => {
    const a = endpointKeyOf(f({ title: 'A', body: 'http://localhost:1337/api/x' }))
    const b = endpointKeyOf(f({ title: 'B', body: 'http://localhost:8080/api/x' }))
    expect(a).toBe('localhost:1337/api/x')
    expect(b).toBe('localhost:8080/api/x')
  })

  it('endpoint 카테고리가 아니면 키가 없다 (취약점끼리 병합 금지)', () => {
    const k = endpointKeyOf(f({ title: 'SQLi in /api/comments', body: 'https://x.com/api/comments', category: 'vuln' }))
    expect(k).toBeNull()
  })
})

describe('vulnKeyOf — 호스트 전역 이슈는 경로 무시', () => {
  const vk = (over: Partial<DedupableFinding> & Pick<DedupableFinding, 'title' | 'body'>): string | null =>
    vulnKeyOf(f({ category: 'vuln', ...over }))

  it('보안 헤더 부재는 엔드포인트가 달라도 호스트로 한 키에 모인다', () => {
    const a = vk({ title: 'Missing security headers (CSP/HSTS) on GET /api/v3/profile', body: 'http://127.0.0.1:8779/api/v3/profile' })
    const b = vk({ title: 'Security response headers absent on /api/v3/items', body: 'http://127.0.0.1:8779/api/v3/items' })
    expect(a).toBe('vulnlike|headers|127.0.0.1:8779')
    expect(a).toBe(b)
  })

  it('와일드카드 CORS 도 클래스가 잡혀 호스트 단위로 모인다', () => {
    const a = vk({ title: 'Wildcard CORS (ACAO: *) on /api/v3/login', body: 'http://127.0.0.1:8779/api/v3/login' })
    const b = vk({ title: 'Wildcard CORS on authenticated /api/v3/profile', body: 'http://127.0.0.1:8779/api/v3/profile' })
    expect(a).toBe('vulnlike|cors|127.0.0.1:8779')
    expect(a).toBe(b)
  })

  it('엔드포인트별 취약점(LFI 등)은 경로가 다르면 그대로 분리된다', () => {
    const a = vk({ title: 'Path traversal (LFI) on GET /api/v3/file', body: 'http://127.0.0.1:8779/api/v3/file' })
    const b = vk({ title: 'Reflected XSS on /api/v3/echo', body: 'http://127.0.0.1:8779/api/v3/echo' })
    expect(a).not.toBe(b)
    expect(a).toContain('/api/v3/file')
  })

  it('호스트 전역 병합은 최고 심각도를 지킨다 (로그인 CORS medium 이 asset CORS low 에 묻히지 않음)', () => {
    const list = [
      f({ category: 'vuln', title: 'Wildcard CORS on static /assets/app.js', body: 'http://127.0.0.1:8779/assets/app.js', severity: 'low' }),
      f({ category: 'vuln', title: 'Wildcard CORS on /api/v3/login lets any origin read the admin token', body: 'http://127.0.0.1:8779/api/v3/login — longer body wins primary', severity: 'medium' })
    ]
    const { merged, mergedAway } = mergeDuplicateEndpoints(list)
    expect(mergedAway).toBe(1)
    expect(merged).toHaveLength(1)
    expect(merged[0].severity).toBe('medium')
  })
})

describe('mergeDuplicateEndpoints', () => {
  it('같은 엔드포인트 3개를 1개로 병합하고 근거는 합친다', () => {
    const list = [
      f({ title: 'A', body: 'GET https://x.com/api/comments — 짧은 관측', evidenceIds: ['1'], tags: ['a'] }),
      f({ title: 'B', body: 'x.com — comments (/api/comments)\n훨씬 더 긴 관측 본문이라 이게 primary 가 된다', evidenceIds: ['2'], tags: ['b'], severity: 'medium' }),
      f({ title: 'C', body: 'https://x.com/api/comments?slug=y 중간', evidenceIds: ['3'] })
    ]
    const { merged, mergedAway } = mergeDuplicateEndpoints(list)
    expect(merged).toHaveLength(1)
    expect(mergedAway).toBe(2)
    expect(merged[0].body).toContain('primary')
    expect([...merged[0].evidenceIds].sort()).toEqual(['1', '2', '3'])
    expect([...merged[0].tags].sort()).toEqual(['a', 'b'])
    expect(merged[0].severity).toBe('medium')
  })

  it('다른 엔드포인트는 병합하지 않는다', () => {
    const list = [
      f({ title: 'A', body: 'https://x.com/api/users' }),
      f({ title: 'B', body: 'https://x.com/api/orders' })
    ]
    const { merged, mergedAway } = mergeDuplicateEndpoints(list)
    expect(merged).toHaveLength(2)
    expect(mergedAway).toBe(0)
  })

  it('vuln 은 클래스가 다르면 같은 URL 이어도 병합하지 않는다', () => {
    const list = [
      f({ title: 'SQLi in /api/q', body: 'https://x.com/api/q', category: 'vuln' }),
      f({ title: 'IDOR in /api/q', body: 'https://x.com/api/q', category: 'vuln' })
    ]
    const { merged } = mergeDuplicateEndpoints(list)
    expect(merged).toHaveLength(2)
  })
})

describe('vulnKeyOf — 같은 싱크를 본 워커들의 중복 병합 (TimeKORP r5 회귀)', () => {
  it('같은 클래스+엔드포인트+파라미터의 XSS 3건(제목 제각각)은 같은 키를 낸다', () => {
    const titles = [
      'Reflected XSS via `format` param — GET http://localhost:1337/ ("Time" app)',
      'Reflected XSS: GET localhost:1337/?format= reflects strftime output into HTML unescaped',
      'Reflected XSS in `format` parameter'
    ]
    const bodies = [
      'The `format` query parameter is reflected into the `<h1>`.',
      '**Target:** `GET http://localhost:1337/?format=<value>`.',
      'Reflected XSS confirmed on GET http://localhost:1337/?format=.'
    ]
    const keys = new Set(
      titles.map((title, i) => vulnKeyOf(f({ title, body: bodies[i], category: 'vuln', tags: ['xss'] })))
    )
    expect(keys.size).toBe(1)
  })

  it('XSS 3건 병합: 1건 남고 severity 최대·validated 승격·근거 합집합', () => {
    const list = [
      f({
        title: 'Reflected XSS via `format` param',
        body: 'GET http://localhost:1337/?format= 가 이스케이프 없이 반사 — 관측 A',
        category: 'vuln',
        severity: 'high',
        status: 'validated',
        evidenceIds: ['1']
      }),
      f({
        title: 'Reflected XSS: GET http://localhost:1337/?format= unescaped',
        body: '관측 B',
        category: 'vuln',
        severity: 'high',
        evidenceIds: ['2']
      }),
      f({
        title: 'Reflected XSS in `format` parameter',
        body: 'http://localhost:1337/?format=<script> 반사 확인 — 관측 C',
        category: 'vuln',
        severity: 'medium',
        evidenceIds: ['3']
      })
    ]
    const { merged, mergedAway } = mergeDuplicateEndpoints(list)
    expect(merged).toHaveLength(1)
    expect(mergedAway).toBe(2)
    expect(merged[0].severity).toBe('high')
    expect(merged[0].status).toBe('validated')
    expect([...merged[0].evidenceIds].sort()).toEqual(['1', '2', '3'])
  })

  it('보안 헤더 누락 candidate+validated 쌍도 하나로 병합된다', () => {
    const list = [
      f({
        title: 'Missing security headers (no CSP / HSTS / X-Frame-Options / nosniff) — http://localhost:1337/',
        body: 'No Content-Security-Policy (high).',
        category: 'vuln',
        severity: 'medium',
        status: 'validated'
      }),
      f({
        title: 'localhost:1337 ("Time" app) ships no security response headers (CSP/HSTS/XFO/nosniff)',
        body: '`http://localhost:1337/` document responses carry none.',
        category: 'vuln',
        severity: 'medium',
        status: 'candidate',
        tags: ['headers', 'csp']
      })
    ]
    const { merged } = mergeDuplicateEndpoints(list)
    expect(merged).toHaveLength(1)
    expect(merged[0].status).toBe('validated')
  })

  it('같은 XSS 클래스여도 파라미터가 다르면 별개 발견이다', () => {
    const a = vulnKeyOf(f({ title: 'XSS in `q` parameter', body: 'GET https://x.com/search?q=1', category: 'vuln' }))
    const b = vulnKeyOf(f({ title: 'XSS in `lang` parameter', body: 'GET https://x.com/search?lang=en', category: 'vuln' }))
    expect(a).not.toBe(b)
  })

  it('클래스를 알 수 없는 vuln 은 병합 대상이 아니다', () => {
    expect(vulnKeyOf(f({ title: '이상한 동작 관찰', body: 'https://x.com/a', category: 'vuln' }))).toBeNull()
  })

  it('auth/secret 카테고리는 여전히 병합하지 않는다', () => {
    const list = [
      f({ title: 'broken auth', body: 'https://x.com/a', category: 'auth' }),
      f({ title: 'broken auth again', body: 'https://x.com/a', category: 'auth' })
    ]
    expect(mergeDuplicateEndpoints(list).merged).toHaveLength(2)
  })

  it("category 'other' 의 헤더 누락 중복도 병합된다 — Testimonial 회귀", () => {
    const list = [
      f({
        title: 'Missing security headers on localhost:1337 (no CSP/HSTS/X-Frame-Options/nosniff)',
        body: 'security_inspect on http://localhost:1337/ returned none.',
        category: 'other',
        severity: 'medium'
      }),
      f({
        title: 'Missing HTTP security headers on localhost:1337 app root (no CSP, HSTS, X-Frame-Options, nosniff)',
        body: 'all responses from http://localhost:1337/ omit them.',
        category: 'other',
        severity: 'low'
      })
    ]
    const { merged } = mergeDuplicateEndpoints(list)
    expect(merged).toHaveLength(1)
    expect(merged[0].severity).toBe('medium')
  })
})

describe('isScanNoise', () => {
  it('false_positive 상태는 노이즈다', () => {
    expect(isScanNoise({ status: 'false_positive', tags: [] })).toBe(true)
  })

  it('negative 태그는 상태와 무관하게 노이즈다', () => {
    expect(isScanNoise({ status: 'candidate', tags: ['broken_auth', 'negative'] })).toBe(true)
    expect(isScanNoise({ status: 'validated', tags: ['Negative'] })).toBe(true)
  })

  it('진짜 후보/검증 발견은 노이즈가 아니다', () => {
    expect(isScanNoise({ status: 'candidate', tags: ['ssrf'] })).toBe(false)
    expect(isScanNoise({ status: 'validated', tags: ['sqli'] })).toBe(false)
  })
})
