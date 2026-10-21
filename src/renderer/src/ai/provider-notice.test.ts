import { describe, it, expect } from 'vitest'

import { detectSafeguardNotice } from './provider-notice'

describe('detectSafeguardNotice', () => {
  it('safeguard가 응답을 멈춘 노티스를 감지한다', () => {
    expect(
      detectSafeguardNotice("Fable 5.1's safeguards stopped the response above · continuing once with that noted")
    ).toBe(true)
  })

  it('모델 폴백 노티스를 감지한다', () => {
    expect(
      detectSafeguardNotice(
        'Model fallback: claude-fable-5-1 declined this request (cyber); retried with claude-opus-4-8.'
      )
    ).toBe(true)
  })

  it('safeguards flagged this message 노티스를 감지한다', () => {
    expect(detectSafeguardNotice("Opus 4.8's safeguards flagged this message.")).toBe(true)
  })

  it('노티스가 답변 본문에 섞여 있어도 감지한다', () => {
    const mixed =
      "Sure, here is what I found.\n\nModel fallback: claude-fable-5-1 declined this request (cyber); retried with claude-opus-4-8.\n\nThe endpoint is /api/v2/search."
    expect(detectSafeguardNotice(mixed)).toBe(true)
  })

  it('일반 답변은 감지하지 않는다', () => {
    expect(detectSafeguardNotice('The login endpoint is POST /api/auth with a bearer token.')).toBe(false)
  })

  it('safeguard를 단순 언급한 문장은 감지하지 않는다', () => {
    expect(
      detectSafeguardNotice('You could add a safeguard here to validate the input before sending.')
    ).toBe(false)
  })

  it('빈 입력은 false', () => {
    expect(detectSafeguardNotice('')).toBe(false)
    expect(detectSafeguardNotice(undefined)).toBe(false)
    expect(detectSafeguardNotice(null)).toBe(false)
  })
})
