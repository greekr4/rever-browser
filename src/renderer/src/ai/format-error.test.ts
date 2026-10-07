import { describe, expect, it } from 'vitest'
import { formatConnectionError } from './format-error'

describe('formatConnectionError', () => {
  it('rate limit 에러에는 모델 사용 불가 안내를 덧붙인다', () => {
    const out = formatConnectionError(new Error('Internal error: API Error: Rate limit reached'))
    expect(out).toContain('Rate limit reached')
    expect(out).toContain('switch model')
  })
  it('ENOENT는 바이너리 누락 안내로 바꾼다', () => {
    expect(formatConnectionError(new Error('spawn claude-agent-acp ENOENT'))).toMatch(/not found/)
  })
  it('그 외 메시지는 그대로 돌려준다', () => {
    expect(formatConnectionError(new Error('boom'))).toBe('boom')
  })
})
