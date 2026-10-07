import { describe, expect, it } from 'vitest'

import {
  chooseAllowOption,
  extractMethod,
  extractTargetHost,
  fallbackOption,
  gatedToolName,
  isGatedToolRequest,
  isRiskyToolRequest,
  matchedRiskyTool,
  sanitizeSelection
} from './risky-tools'
import type { RequestPermissionRequest } from '@agentclientprotocol/sdk'

// claude-agent-acp의 실제 옵션 세트 (acp-agent.js canUseTool)
const ACP_OPTIONS = [
  { kind: 'allow_always', name: 'Always Allow', optionId: 'allow_always' },
  { kind: 'allow_once', name: 'Allow', optionId: 'allow' },
  { kind: 'reject_once', name: 'Reject', optionId: 'reject' }
]

function req(toolCallId: string, title: string, rawInput?: unknown): RequestPermissionRequest {
  return {
    sessionId: 's',
    options: ACP_OPTIONS,
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

    it('실제 툴 스키마의 overrides/modifications.method를 읽는다', () => {
      expect(matchedRiskyTool(req('x', 'replay_request', { overrides: { method: 'GET' } }))).toBeNull()
      expect(matchedRiskyTool(req('x', 'replay_request', { overrides: { method: 'POST' } }))).toBe('replay_request')
      expect(matchedRiskyTool(req('x', 'repeater_send', { modifications: { method: 'get' } }))).toBeNull()
      expect(matchedRiskyTool(req('x', 'repeater_send', { modifications: { method: 'PUT' } }))).toBe('repeater_send')
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

  describe('isGatedToolRequest', () => {
    it('메서드와 무관하게 게이트 대상 툴이면 true다', () => {
      expect(isGatedToolRequest(req('x', 'mcp__rever-traffic__repeater_send', { modifications: { method: 'GET' } }))).toBe(true)
      expect(isGatedToolRequest(req('x', 'mcp__rever-traffic__intruder_run'))).toBe(true)
      expect(isGatedToolRequest(req('x', 'mcp__rever-traffic__list_requests'))).toBe(false)
    })
  })

  describe('셸 HTTP (Bash로 curl 등 실행)', () => {
    // claude-agent-acp는 Bash 호출의 title을 명령어 자체로, rawInput을 {command}로 보낸다
    const bash = (command: string) => req('toolu_1', command, { command, description: 'd' })

    it('curl/wget 실행은 메서드와 무관하게 위험이다', () => {
      expect(matchedRiskyTool(bash('curl -s -X POST http://localhost:8779/'))).toBe('shell_http')
      expect(matchedRiskyTool(bash('curl https://example.com/'))).toBe('shell_http')
      expect(matchedRiskyTool(bash('wget -qO- https://example.com'))).toBe('shell_http')
    })

    it('루프·서브셸·파이프 안의 curl도 잡는다', () => {
      expect(matchedRiskyTool(bash('for i in $(seq 1 3); do c=$(curl -s http://x.io/); done'))).toBe('shell_http')
      expect(matchedRiskyTool(bash('echo hi && curl http://x.io'))).toBe('shell_http')
    })

    it('인라인 Python/Node HTTP 스크립트도 잡는다', () => {
      expect(matchedRiskyTool(bash('python3 -c "import requests; requests.post(\'http://x.io\')"'))).toBe('shell_http')
      expect(matchedRiskyTool(bash("node -e \"fetch('http://x.io',{method:'POST'})\""))).toBe('shell_http')
    })

    it('curl이 인자로만 등장하는 명령은 위험이 아니다', () => {
      expect(matchedRiskyTool(bash('grep -rn curl src/'))).toBeNull()
      expect(matchedRiskyTool(bash('git status'))).toBeNull()
      expect(matchedRiskyTool(bash('cat curl-notes.md'))).toBeNull()
    })

    it('게이트 대상이라 allow_once·세션 허용 키를 쓰고, 대상 host를 보여준다', () => {
      const r = bash('curl -X POST http://localhost:8779/api')
      expect(gatedToolName(r)).toBe('shell_http')
      expect(chooseAllowOption(r)).toBe('allow')
      expect(extractTargetHost(r.toolCall?.rawInput)).toBe('localhost:8779')
    })
  })

  describe('gatedToolName', () => {
    it('메서드와 무관하게 게이트 툴 이름을 돌려준다', () => {
      expect(gatedToolName(req('x', 'mcp__rever-traffic__repeater_send', { modifications: { method: 'GET' } }))).toBe('repeater_send')
      expect(gatedToolName(req('x', 'mcp__rever-traffic__lfi_probe'))).toBe('lfi_probe')
      expect(gatedToolName(req('x', 'mcp__rever-traffic__list_requests'))).toBeNull()
    })
  })

  describe('chooseAllowOption', () => {
    it('게이트 대상 툴은 세션 규칙이 남지 않도록 allow_once를 고른다', () => {
      expect(chooseAllowOption(req('x', 'intruder_run'))).toBe('allow')
      // GET 자동승인이 allow_always면 이후 POST까지 세션 전체가 허용돼 버린다
      expect(chooseAllowOption(req('x', 'repeater_send', { modifications: { method: 'GET' } }))).toBe('allow')
    })
    it('일반 툴은 기존처럼 allow_always를 고른다', () => {
      expect(chooseAllowOption(req('x', 'list_requests'))).toBe('allow_always')
    })
  })

  describe('sanitizeSelection', () => {
    it('게이트 대상 툴에서 고른 allow_always를 allow_once로 낮춘다', () => {
      expect(sanitizeSelection(req('x', 'burst_send'), 'allow_always')).toBe('allow')
    })
    it('그 외 선택은 그대로 둔다', () => {
      expect(sanitizeSelection(req('x', 'burst_send'), 'reject')).toBe('reject')
      expect(sanitizeSelection(req('x', 'list_requests'), 'allow_always')).toBe('allow_always')
    })
  })

  describe('fallbackOption', () => {
    it('UI 왕복이 실패하면 위험 툴은 거부한다', () => {
      expect(fallbackOption(req('x', 'lfi_probe'))).toBe('reject')
      expect(fallbackOption(req('x', 'replay_request', { overrides: { method: 'DELETE' } }))).toBe('reject')
    })
    it('위험하지 않은 요청은 허용하되 게이트 툴은 allow_once로 남긴다', () => {
      expect(fallbackOption(req('x', 'list_requests'))).toBe('allow_always')
      expect(fallbackOption(req('x', 'replay_request', { overrides: { method: 'GET' } }))).toBe('allow')
    })
  })

  describe('extractTargetHost', () => {
    it('url에서 host를 뽑는다', () => {
      expect(extractTargetHost({ url: 'https://api.example.com/v3/order' })).toBe('api.example.com')
      expect(extractTargetHost({ request: { url: 'https://x.io:8443/a' } })).toBe('x.io:8443')
    })
    it('overrides/modifications.url에서도 host를 뽑는다', () => {
      expect(extractTargetHost({ overrides: { url: 'https://evil.test/x' } })).toBe('evil.test')
      expect(extractTargetHost({ modifications: { url: 'http://127.0.0.1:8777/a' } })).toBe('127.0.0.1:8777')
    })
    it('host 필드나 잘못된 url도 처리한다', () => {
      expect(extractTargetHost({ host: 'cdn.example.com' })).toBe('cdn.example.com')
      expect(extractTargetHost({ url: 'not a url' })).toBeNull()
      expect(extractTargetHost(42)).toBeNull()
    })
  })
})
