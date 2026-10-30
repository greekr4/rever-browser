import { afterEach, describe, expect, it } from 'vitest'

import {
  cancelPendingPermissions,
  hasPendingPermission,
  requestPermissionFromUser
} from './acp-permission'
import { addRedTeamScope, setRedTeamScan } from './red-team-mode'
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

function toolReq(sessionId: string, title: string, rawInput: unknown): RequestPermissionRequest {
  return {
    sessionId,
    options: [
      { kind: 'allow_once', name: 'Allow', optionId: 'allow' },
      { kind: 'reject_once', name: 'Reject', optionId: 'reject' }
    ],
    toolCall: { toolCallId: 't', title, rawInput }
  } as unknown as RequestPermissionRequest
}

const ALLOW = { outcome: { outcome: 'selected', optionId: 'allow' } }

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

  describe('Red Team 자동승인', () => {
    afterEach(() => setRedTeamScan(false))

    it('범위 내 호스트의 위험 probe 를 자동승인한다', async () => {
      setRedTeamScan(true, 'http://localhost:8080/my')
      const r = await requestPermissionFromUser(
        toolReq('rt1', 'mcp__rever-traffic__path_probe', { baseUrl: 'http://localhost:8080/' })
      )
      expect(r).toEqual(ALLOW)
      expect(hasPendingPermission('rt1')).toBe(false)
    })

    it('host 를 못 뽑는 replay_request(requestId)도 자동승인한다 (서버 scope 가 막음)', async () => {
      setRedTeamScan(true, 'http://localhost:8080/my')
      const r = await requestPermissionFromUser(
        toolReq('rt2', 'mcp__rever-traffic__replay_request', { requestId: '27237.71' })
      )
      expect(r).toEqual(ALLOW)
    })

    it('범위 밖 포트(8090)는 자동승인하지 않고 프롬프트한다', () => {
      setRedTeamScan(true, 'http://localhost:8080/my')
      void requestPermissionFromUser(
        toolReq('rt3', 'mcp__rever-traffic__path_probe', { baseUrl: 'http://localhost:8090/' })
      )
      expect(hasPendingPermission('rt3')).toBe(true)
      cancelPendingPermissions('rt3')
    })

    it('scope_add 된 API 호스트(:3000)의 replay 는 자동승인한다', async () => {
      setRedTeamScan(true, 'http://localhost:8080/my')
      addRedTeamScope(['http://localhost:3000/'])
      const r = await requestPermissionFromUser(
        toolReq('rt5', 'mcp__rever-traffic__replay_request', {
          requestId: '1',
          url: 'http://localhost:3000/v1/users/1'
        })
      )
      expect(r).toEqual(ALLOW)
    })

    it('셸 HTTP(curl)는 Red Team 이어도 프롬프트한다', () => {
      setRedTeamScan(true, 'http://localhost:8080/my')
      void requestPermissionFromUser(toolReq('rt4', 'Bash', { command: 'curl http://localhost:8080/x' }))
      expect(hasPendingPermission('rt4')).toBe(true)
      cancelPendingPermissions('rt4')
    })
  })
})
