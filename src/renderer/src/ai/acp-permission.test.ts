import { describe, expect, it } from 'vitest'

import {
  cancelPendingPermissions,
  hasPendingPermission,
  requestPermissionFromUser
} from './acp-permission'
import type { RequestPermissionRequest } from '@agentclientprotocol/sdk'


function riskyReq(sessionId: string): RequestPermissionRequest {
  return {
    sessionId,
    options: [
      { kind: 'allow_once', name: 'Allow', optionId: 'allow' },
      { kind: 'reject_once', name: 'Reject', optionId: 'reject' }
    ],
    toolCall: { toolCallId: 't', title: 'mcp__rever-traffic__burst_send', rawInput: {} }
  } as unknown as RequestPermissionRequest
}

describe('acp-permission', () => {
  describe('cancelPendingPermissions', () => {
    it('Stop한 세션의 대기 중 권한 요청을 cancelled로 응답하고 큐에서 뺀다', async () => {
      const pending = requestPermissionFromUser(riskyReq('s1'))
      expect(hasPendingPermission('s1')).toBe(true)

      cancelPendingPermissions('s1')

      await expect(pending).resolves.toEqual({ outcome: { outcome: 'cancelled' } })
      expect(hasPendingPermission('s1')).toBe(false)
    })

    it('다른 세션의 요청은 건드리지 않는다', async () => {
      const other = requestPermissionFromUser(riskyReq('s2'))
      cancelPendingPermissions('s1')
      expect(hasPendingPermission('s2')).toBe(true)
      cancelPendingPermissions('s2')
      await expect(other).resolves.toEqual({ outcome: { outcome: 'cancelled' } })
    })
  })
})
