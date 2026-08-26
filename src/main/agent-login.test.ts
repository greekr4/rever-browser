import { describe, expect, it } from 'vitest'

import { readClaudeLogin, readCodexLogin } from './agent-login'

const NOW = 1_700_000_000_000
const HOUR = 3_600_000

function claudeBlob(over: Record<string, unknown> = {}): string {
  return JSON.stringify({
    claudeAiOauth: {
      accessToken: 'at',
      refreshToken: 'rt',
      expiresAt: NOW + HOUR,
      refreshTokenExpiresAt: NOW + 30 * 24 * HOUR,
      subscriptionType: 'max',
      ...over
    }
  })
}

describe('readClaudeLogin', () => {
  it('유효한 액세스 토큰이면 로그인으로 본다', () => {
    expect(readClaudeLogin(claudeBlob(), NOW)).toEqual({ loggedIn: true, plan: 'max' })
  })

  it('액세스 토큰이 만료돼도 리프레시 토큰이 살아있으면 로그인이다', () => {
    const raw = claudeBlob({ expiresAt: NOW - HOUR })
    expect(readClaudeLogin(raw, NOW)).toEqual({ loggedIn: true, plan: 'max' })
  })

  it('두 토큰이 모두 만료되면 로그인이 아니다', () => {
    const raw = claudeBlob({ expiresAt: NOW - HOUR, refreshTokenExpiresAt: NOW - HOUR })
    expect(readClaudeLogin(raw, NOW)).toEqual({ loggedIn: false, plan: null })
  })

  it('만료 시각이 없으면 토큰 존재만으로 판단한다', () => {
    const raw = claudeBlob({ expiresAt: undefined, refreshTokenExpiresAt: undefined })
    expect(readClaudeLogin(raw, NOW).loggedIn).toBe(true)
  })

  it('subscriptionType이 없으면 plan은 null이다', () => {
    const raw = claudeBlob({ subscriptionType: undefined })
    expect(readClaudeLogin(raw, NOW)).toEqual({ loggedIn: true, plan: null })
  })

  it('액세스 토큰이 비었으면 로그인이 아니다', () => {
    expect(readClaudeLogin(claudeBlob({ accessToken: '' }), NOW).loggedIn).toBe(false)
  })

  it('claudeAiOauth 키가 없으면 로그인이 아니다', () => {
    expect(readClaudeLogin(JSON.stringify({ mcpOAuth: {} }), NOW).loggedIn).toBe(false)
  })

  it('JSON이 깨졌으면 던지지 않고 로그인 아님으로 처리한다', () => {
    expect(readClaudeLogin('not json', NOW)).toEqual({ loggedIn: false, plan: null })
    expect(readClaudeLogin('', NOW)).toEqual({ loggedIn: false, plan: null })
  })
})

describe('readCodexLogin', () => {
  it('ChatGPT 토큰이 있으면 로그인이다', () => {
    const raw = JSON.stringify({
      auth_mode: 'chatgpt',
      OPENAI_API_KEY: null,
      tokens: { access_token: 'at', refresh_token: 'rt' }
    })
    expect(readCodexLogin(raw)).toEqual({ loggedIn: true, plan: 'chatgpt' })
  })

  it('API 키만 있어도 로그인이다', () => {
    const raw = JSON.stringify({ OPENAI_API_KEY: 'sk-x', tokens: null })
    expect(readCodexLogin(raw)).toEqual({ loggedIn: true, plan: 'api-key' })
  })

  it('토큰도 키도 없으면 로그인이 아니다', () => {
    const raw = JSON.stringify({ auth_mode: 'chatgpt', OPENAI_API_KEY: null, tokens: {} })
    expect(readCodexLogin(raw)).toEqual({ loggedIn: false, plan: null })
  })

  it('JSON이 깨졌으면 던지지 않고 로그인 아님으로 처리한다', () => {
    expect(readCodexLogin('{{')).toEqual({ loggedIn: false, plan: null })
  })
})
