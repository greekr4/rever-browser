import { describe, expect, it } from 'vitest'

import {
  extractMethod,
  extractTargetHost,
  isRiskyToolRequest,
  matchedRiskyTool
} from './risky-tools'
import type { RequestPermissionRequest } from '@agentclientprotocol/sdk'

function req(toolCallId: string, title: string, rawInput?: unknown): RequestPermissionRequest {
  return {
    sessionId: 's',
    options: [],
    toolCall: { toolCallId, title, rawInput }
  } as unknown as RequestPermissionRequest
}

describe('risky-tools', () => {
  describe('matchedRiskyTool', () => {
    it('항상 위험한 툴은 메서드와 무관하게 매칭된다', () => {
      expect(matchedRiskyTool(req('mcp__rever-traffic__intruder_run', 'Intruder'))).toBe('intruder_run')
      expect(matchedRiskyTool(req('x', 'Running burst_send'))).toBe('burst_send')
    })

    it('메서드 게이트 툴은 GET/HEAD면 안전으로 통과한다', () => {
      expect(matchedRiskyTool(req('mcp__rever-traffic__replay_request', 't', { method: 'GET' }))).toBeNull()
      expect(matchedRiskyTool(req('x', 'repeater_send', { method: 'head' }))).toBeNull()
    })

    it('메서드 게이트 툴은 비멱등 메서드면 위험으로 매칭된다', () => {
      expect(matchedRiskyTool(req('x', 'replay_request', { method: 'POST' }))).toBe('replay_request')
      expect(matchedRiskyTool(req('x', 'repeater_send', { request: { method: 'DELETE' } }))).toBe('repeater_send')
    })

    it('메서드를 알 수 없으면 안전하게 위험으로 취급한다', () => {
      expect(matchedRiskyTool(req('x', 'replay_request', {}))).toBe('replay_request')
      expect(matchedRiskyTool(req('x', 'replay_request'))).toBe('replay_request')
    })

    it('위험 목록에 없는 툴은 매칭되지 않는다', () => {
      expect(matchedRiskyTool(req('mcp__rever-traffic__list_requests', 'List'))).toBeNull()
      expect(isRiskyToolRequest(req('x', 'browser_snapshot'))).toBe(false)
    })
  })

  describe('extractMethod', () => {
    it('평면/중첩 method를 모두 읽는다', () => {
      expect(extractMethod({ method: 'POST' })).toBe('POST')
      expect(extractMethod({ request: { method: 'PUT' } })).toBe('PUT')
      expect(extractMethod({})).toBeNull()
      expect(extractMethod(null)).toBeNull()
    })
  })

  describe('extractTargetHost', () => {
    it('url에서 host를 뽑는다', () => {
      expect(extractTargetHost({ url: 'https://api.example.com/v3/order' })).toBe('api.example.com')
      expect(extractTargetHost({ request: { url: 'https://x.io:8443/a' } })).toBe('x.io:8443')
    })
    it('host 필드나 잘못된 url도 처리한다', () => {
      expect(extractTargetHost({ host: 'cdn.example.com' })).toBe('cdn.example.com')
      expect(extractTargetHost({ url: 'not a url' })).toBeNull()
      expect(extractTargetHost(42)).toBeNull()
    })
  })
})
