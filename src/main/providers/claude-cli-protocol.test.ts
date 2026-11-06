import { describe, expect, it } from 'vitest'

import {
  claudeCliArgs,
  modelState,
  newTranslateState,
  permissionResponse,
  toPermissionRequest,
  translate,
  turnOutcome
} from './claude-cli-protocol'

const SID = 'claude-cli:s1'
const upd = (msg: Record<string, unknown>, state = newTranslateState()) =>
  translate(msg, SID, state).map((n) => (n as unknown as { update: Record<string, unknown> }).update)

describe('claudeCliArgs', () => {
  it('구독 인증을 지키고 권한 요청을 호스트로 보낸다', () => {
    const args = claudeCliArgs({ mcpUrl: 'http://127.0.0.1:1/mcp', authHeader: 'Bearer t' })
    expect(args).not.toContain('--bare')
    expect(args.join(' ')).toContain('--permission-prompt-tool stdio')
    const mcp = JSON.parse(args[args.indexOf('--mcp-config') + 1])
    expect(mcp.mcpServers['rever-traffic']).toMatchObject({ type: 'http', url: 'http://127.0.0.1:1/mcp' })
    expect(args).not.toContain('--model')
    expect(claudeCliArgs({ mcpUrl: 'u', authHeader: 'a', model: 'opus' }).slice(-2)).toEqual(['--model', 'opus'])
  })
})

describe('translate', () => {
  it('텍스트·생각 델타를 메시지·생각 청크로 바꾼다', () => {
    expect(upd({ type: 'stream_event', event: { type: 'content_block_delta', delta: { type: 'text_delta', text: 'hi' } } })).toEqual([
      { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'hi' } }
    ])
    expect(upd({ type: 'stream_event', event: { type: 'content_block_delta', delta: { type: 'thinking_delta', thinking: 'hm' } } })[0]).toMatchObject({
      sessionUpdate: 'agent_thought_chunk'
    })
    expect(upd({ type: 'stream_event', event: { type: 'content_block_delta', delta: { type: 'signature_delta', signature: 'x' } } })).toEqual([])
  })

  it('도구 호출을 시작 → 입력 → 결과 순서로 알린다', () => {
    const s = newTranslateState()
    const id = 'toolu_1'
    expect(upd({ type: 'stream_event', event: { type: 'content_block_start', content_block: { type: 'tool_use', id, name: 'mcp__rever-traffic__list_requests', input: {} } } }, s)).toEqual([
      { sessionUpdate: 'tool_call', toolCallId: id, title: 'mcp__rever-traffic__list_requests', status: 'pending' }
    ])
    expect(upd({ type: 'assistant', message: { content: [{ type: 'text', text: 'dup' }, { type: 'tool_use', id, name: 'x', input: { limit: 1 } }] } }, s)).toEqual([
      { sessionUpdate: 'tool_call_update', toolCallId: id, rawInput: { limit: 1 }, status: 'in_progress' }
    ])
    const done = upd({ type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: id, content: [{ type: 'text', text: '[1]' }] }] } }, s)
    expect(done[0]).toMatchObject({ sessionUpdate: 'tool_call_update', toolCallId: id, status: 'completed', rawOutput: '[1]' })
  })

  it('실패한 도구 결과는 failed와 에러 문구를 담는다', () => {
    const [u] = upd({ type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 't', is_error: true, content: 'denied' }] } })
    expect(u).toMatchObject({ status: 'failed', content: [{ type: 'content', content: { type: 'text', text: 'denied' } }] })
  })

  it('ToolSearch 같은 내부 도구는 숨긴다', () => {
    const s = newTranslateState()
    expect(upd({ type: 'stream_event', event: { type: 'content_block_start', content_block: { type: 'tool_use', id: 'ts', name: 'ToolSearch' } } }, s)).toEqual([])
    expect(upd({ type: 'assistant', message: { content: [{ type: 'tool_use', id: 'ts', name: 'ToolSearch', input: {} }] } }, s)).toEqual([])
    expect(upd({ type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 'ts', content: [] }] } }, s)).toEqual([])
  })

  it('시스템·결과 메시지는 업데이트를 만들지 않는다', () => {
    expect(upd({ type: 'system', subtype: 'init' })).toEqual([])
    expect(upd({ type: 'result', subtype: 'success' })).toEqual([])
  })
})

describe('turnOutcome', () => {
  it('성공·취소·실패를 구분한다', () => {
    expect(turnOutcome({ subtype: 'success', stop_reason: 'end_turn' }, false)).toEqual({ stopReason: 'end_turn' })
    expect(turnOutcome({ subtype: 'error_during_execution' }, true)).toEqual({ stopReason: 'cancelled' })
    expect(turnOutcome({ subtype: 'error_max_turns', errors: ['too many'] }, false)).toEqual({ error: 'Claude Code turn failed: too many' })
  })
})

describe('권한 왕복', () => {
  const req = { subtype: 'can_use_tool', tool_name: 'mcp__rever-traffic__intruder_run', input: { n: 1 }, tool_use_id: 'toolu_9' }

  it('can_use_tool을 위험 도구 판별이 가능한 ACP 요청으로 바꾼다', () => {
    const r = toPermissionRequest(SID, req) as unknown as { toolCall: { title: string; rawInput: unknown }; options: { kind: string }[] }
    expect(r.toolCall.title).toContain('intruder_run')
    expect(r.toolCall.rawInput).toEqual({ n: 1 })
    expect(r.options.map((o) => o.kind)).toEqual(['allow_once', 'allow_always', 'reject_once'])
  })

  it('허용은 원래 입력과 함께 allow, 거절은 deny로 답한다', () => {
    const allow = JSON.parse(permissionResponse('r1', { outcome: { outcome: 'selected', optionId: 'allow_always' } }, { n: 1 }))
    expect(allow.response).toEqual({ subtype: 'success', request_id: 'r1', response: { behavior: 'allow', updatedInput: { n: 1 } } })
    const deny = JSON.parse(permissionResponse('r2', { outcome: { outcome: 'selected', optionId: 'reject' } }, {}))
    expect(deny.response.response.behavior).toBe('deny')
    expect(JSON.parse(permissionResponse('r3', { outcome: { outcome: 'cancelled' } } as never, {})).response.response.behavior).toBe('deny')
  })
})

describe('modelState', () => {
  it('initialize 모델 목록을 모델 선택기 형식으로 바꾼다', () => {
    expect(modelState([{ value: 'opus', displayName: 'Opus 5.5', description: 'x' }], 'opus')).toEqual({
      availableModels: [{ modelId: 'opus', name: 'Opus 5.5', description: 'x' }],
      currentModelId: 'opus'
    })
  })
})
