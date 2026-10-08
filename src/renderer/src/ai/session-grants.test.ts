import { afterEach, describe, expect, it } from 'vitest'

import { clearSessionGrants, grantSession, hasSessionGrant } from './session-grants'
import type { RequestPermissionRequest } from '@agentclientprotocol/sdk'

function req(sessionId: string, title: string, rawInput?: unknown): RequestPermissionRequest {
  return {
    sessionId,
    options: [],
    toolCall: { toolCallId: 't', title, rawInput }
  } as unknown as RequestPermissionRequest
}

afterEach(() => clearSessionGrants())

describe('session-grants', () => {
  it('허용한 툴은 같은 세션의 다음 호출에서 grant가 있다', () => {
    grantSession(req('s1', 'mcp__rever-traffic__repeater_send', { modifications: { method: 'POST' } }))
    expect(hasSessionGrant(req('s1', 'mcp__rever-traffic__repeater_send', { modifications: { method: 'DELETE' } }))).toBe(true)
  })

  it('다른 세션이나 다른 툴에는 번지지 않는다', () => {
    grantSession(req('s1', 'mcp__rever-traffic__repeater_send'))
    expect(hasSessionGrant(req('s2', 'mcp__rever-traffic__repeater_send'))).toBe(false)
    expect(hasSessionGrant(req('s1', 'mcp__rever-traffic__intruder_run'))).toBe(false)
  })

  it('게이트 대상이 아닌 툴은 grant를 저장하지 않는다', () => {
    grantSession(req('s1', 'mcp__rever-traffic__list_requests'))
    expect(hasSessionGrant(req('s1', 'mcp__rever-traffic__list_requests'))).toBe(false)
  })

  it('clearSessionGrants로 모두 초기화된다', () => {
    grantSession(req('s1', 'mcp__rever-traffic__burst_send'))
    clearSessionGrants()
    expect(hasSessionGrant(req('s1', 'mcp__rever-traffic__burst_send'))).toBe(false)
  })
})
