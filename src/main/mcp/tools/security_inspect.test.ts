import { describe, it, expect } from 'vitest'
import { analyze, pickInspectable } from './security_inspect'

// 2026-11-02 arqoven.com 실측 헤더 — 이 세트에서 "헤더 전무" 오탐이 발생했다
const ARQOVEN_HEADERS: Record<string, string> = {
  'content-security-policy':
    "default-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
  'strict-transport-security': 'max-age=15552000',
  'x-frame-options': 'DENY',
  'x-content-type-options': 'nosniff'
}

describe('pickInspectable', () => {
  it('헤더가 비어 있는 Document를 건너뛰고 헤더 있는 최신 항목을 고른다', () => {
    const store: Record<string, { responseHeaders: Record<string, string> }> = {
      a: { responseHeaders: {} }, // 캐시 히트/실패 탐색 등
      b: { responseHeaders: ARQOVEN_HEADERS }
    }
    const picked = pickInspectable(
      [{ requestId: 'a' }, { requestId: 'b' }],
      (id) => store[id]
    )
    expect(picked?.requestId).toBe('b')
  })

  it(' getRequest가 undefined를 돌려주는(밀려난) 엔트리도 건너뛴다', () => {
    const picked = pickInspectable(
      [{ requestId: 'gone' }, { requestId: 'ok' }],
      (id) => (id === 'ok' ? { responseHeaders: ARQOVEN_HEADERS } : undefined)
    )
    expect(picked?.requestId).toBe('ok')
  })

  it('전부 비어 있으면 undefined (빈 맵 감사 금지)', () => {
    const picked = pickInspectable(
      [{ requestId: 'a' }, { requestId: 'b' }],
      () => ({ responseHeaders: {} })
    )
    expect(picked).toBeUndefined()
  })
})

describe('analyze (arqoven 오탐 회귀)', () => {
  it('헤더가 온전히 있으면 CSP/HSTS/XFO/nosniff 이슈를 내지 않는다', () => {
    const issues = analyze(ARQOVEN_HEADERS)
    const headers = issues.map((i) => i.header)
    expect(headers).not.toContain('content-security-policy')
    expect(headers).not.toContain('strict-transport-security')
    expect(headers).not.toContain('x-frame-options')
    expect(headers).not.toContain('x-content-type-options')
  })
})
