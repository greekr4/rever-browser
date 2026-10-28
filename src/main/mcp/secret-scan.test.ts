import { describe, it, expect } from 'vitest'

import { redact, redactDeep } from './secret-scan'

describe('redact', () => {
  it('Bearer 토큰을 마스킹한다', () => {
    const out = redact('curl -H "Authorization: Bearer 6c70a033abcd1234efgh5678" x')
    expect(out).not.toContain('6c70a033abcd1234efgh5678')
    expect(out).toContain('‹bearer-token:')
  })

  it('JWT를 마스킹한다', () => {
    const jwt = 'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghij'
    expect(redact(`token=${jwt}`)).not.toContain(jwt)
  })

  it('이메일과 휴대폰 번호를 마스킹한다', () => {
    const out = redact('연락처: hong@example.com / 010-1234-5678')
    expect(out).not.toContain('hong@example.com')
    expect(out).not.toContain('010-1234-5678')
    expect(out).toContain('‹email:')
    expect(out).toContain('‹phone-kr:')
  })

  it('OAuth refresh token 필드 값을 마스킹한다', () => {
    const out = redact('"naverRefreshToken":"AAAABBBBCCCCDDDDEEEE1234"')
    expect(out).not.toContain('AAAABBBBCCCCDDDDEEEE1234')
  })

  it('비밀값이 없으면 원문을 보존한다', () => {
    const plain = 'GET /v1/jobs 는 공개 목록입니다'
    expect(redact(plain)).toBe(plain)
  })
})

describe('redactDeep', () => {
  it('중첩된 문자열만 마스킹하고 구조는 유지한다', () => {
    const input = {
      update: { sessionUpdate: 'agent_message_chunk', content: { text: 'Bearer 6c70a033abcd1234efgh' } },
      n: 42
    }
    const out = redactDeep(input) as typeof input
    expect(out.n).toBe(42)
    expect(out.update.sessionUpdate).toBe('agent_message_chunk')
    expect(out.update.content.text).not.toContain('6c70a033abcd1234efgh')
  })
})
