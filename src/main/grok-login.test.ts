import { describe, expect, it } from 'vitest'

import { applyGrokTokenResponse, readGrokLogin } from './grok-login'

const NOW = Date.parse('2026-10-19T00:00:00.000Z')
const HOUR = 3_600_000

function blob(over: Record<string, unknown> = {}): string {
  return JSON.stringify({
    'https://auth.x.ai::client': {
      key: 'access',
      refresh_token: 'refresh',
      expires_at: new Date(NOW + HOUR).toISOString(),
      auth_mode: 'oidc',
      oidc_client_id: 'client',
      oidc_issuer: 'https://auth.x.ai',
      ...over
    }
  })
}

describe('readGrokLogin', () => {
  it('유효한 grok.com 로그인이면 구독으로 본다', () => {
    expect(readGrokLogin(blob(), NOW)).toEqual({ loggedIn: true, plan: 'grok.com' })
  })

  it('액세스 토큰이 만료돼도 리프레시 토큰이 있으면 로그인이다', () => {
    const raw = blob({ expires_at: new Date(NOW - HOUR).toISOString() })
    expect(readGrokLogin(raw, NOW)).toEqual({ loggedIn: true, plan: 'grok.com' })
  })

  it('토큰이 둘 다 없으면 로그인이 아니다', () => {
    expect(readGrokLogin(blob({ key: '', refresh_token: '' }), NOW).loggedIn).toBe(false)
    expect(readGrokLogin('not json', NOW)).toEqual({ loggedIn: false, plan: null })
    expect(readGrokLogin('{}', NOW).loggedIn).toBe(false)
  })
})

describe('applyGrokTokenResponse', () => {
  it('새 액세스 토큰과 만료 시각을 같은 항목에 넣는다', () => {
    const applied = applyGrokTokenResponse(
      blob(),
      { access_token: 'next', refresh_token: 'rotated', expires_in: 120 },
      NOW
    )
    expect(applied?.accessToken).toBe('next')
    const saved = JSON.parse(applied!.raw) as {
      'https://auth.x.ai::client': { key: string; refresh_token: string; expires_at: string }
    }
    const entry = saved['https://auth.x.ai::client']
    expect(entry.key).toBe('next')
    expect(entry.refresh_token).toBe('rotated')
    expect(Date.parse(entry.expires_at) - NOW).toBe(120_000)
  })

  it('액세스 토큰이 없으면 파일을 바꾸지 않는다', () => {
    expect(applyGrokTokenResponse(blob(), {}, NOW)).toBeNull()
  })
})
