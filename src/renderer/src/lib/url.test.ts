import { describe, it, expect } from 'vitest'

import { normalizeAddress } from './url'

describe('normalizeAddress', () => {
  it('로컬호스트는 http로 보정한다 (dev 서버는 https가 아니라서)', () => {
    expect(normalizeAddress('localhost:8080/my')).toBe('http://localhost:8080/my')
    expect(normalizeAddress('localhost')).toBe('http://localhost')
    expect(normalizeAddress('127.0.0.1:8090')).toBe('http://127.0.0.1:8090')
    expect(normalizeAddress('app.localhost:3000')).toBe('http://app.localhost:3000')
  })

  it('일반 도메인은 https로 보정한다', () => {
    expect(normalizeAddress('example.com')).toBe('https://example.com')
    expect(normalizeAddress('naver.com/path')).toBe('https://naver.com/path')
  })

  it('이미 scheme가 있으면 그대로 둔다', () => {
    expect(normalizeAddress('http://localhost:8080')).toBe('http://localhost:8080')
    expect(normalizeAddress('https://example.com')).toBe('https://example.com')
    expect(normalizeAddress('view-source:https://x.com')).toBe('view-source:https://x.com')
    expect(normalizeAddress('about:blank')).toBe('about:blank')
  })

  it('공백은 다듬고 빈 입력은 빈 문자열', () => {
    expect(normalizeAddress('  example.com  ')).toBe('https://example.com')
    expect(normalizeAddress('   ')).toBe('')
  })
})
