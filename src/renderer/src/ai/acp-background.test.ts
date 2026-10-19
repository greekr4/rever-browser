import { describe, expect, it } from 'vitest'

import { endsBackgroundTurn, startsBackgroundTurn } from './acp-background'

import type { SessionUpdate } from '@agentclientprotocol/sdk'

const u = (update: Record<string, unknown>): SessionUpdate => update as unknown as SessionUpdate

describe('startsBackgroundTurn', () => {
  it('에이전트 텍스트·생각·도구 호출이면 자율 턴의 시작으로 본다', () => {
    expect(
      startsBackgroundTurn(u({ sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'done' } }))
    ).toBe(true)
    expect(
      startsBackgroundTurn(u({ sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text: 'hm' } }))
    ).toBe(true)
    expect(startsBackgroundTurn(u({ sessionUpdate: 'tool_call', toolCallId: 't1', title: 'Read' }))).toBe(true)
  })

  it('빈 텍스트 청크는 턴을 열지 않는다', () => {
    expect(
      startsBackgroundTurn(u({ sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: '' } }))
    ).toBe(false)
  })

  it('명령 목록·사용량·이전 도구의 상태 갱신은 턴을 열지 않는다', () => {
    expect(startsBackgroundTurn(u({ sessionUpdate: 'available_commands_update', availableCommands: [] }))).toBe(false)
    expect(startsBackgroundTurn(u({ sessionUpdate: 'usage_update', used: 1, size: 2 }))).toBe(false)
    expect(
      startsBackgroundTurn(u({ sessionUpdate: 'tool_call_update', toolCallId: 'old', status: 'completed' }))
    ).toBe(false)
    expect(startsBackgroundTurn(u({ sessionUpdate: 'rever_pulse' }))).toBe(false)
  })
})

describe('endsBackgroundTurn', () => {
  it('비용이 붙은 usage_update(턴 결과)면 턴이 끝난 것으로 본다', () => {
    expect(
      endsBackgroundTurn(
        u({ sessionUpdate: 'usage_update', used: 10, size: 200, cost: { amount: 0.1, currency: 'USD' } })
      )
    ).toBe(true)
  })

  it('턴 도중의 usage_update(비용 없음)는 끝으로 보지 않는다', () => {
    expect(endsBackgroundTurn(u({ sessionUpdate: 'usage_update', used: 10, size: 200 }))).toBe(false)
    expect(
      endsBackgroundTurn(u({ sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'x' } }))
    ).toBe(false)
  })
})
