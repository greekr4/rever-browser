import { describe, expect, it } from 'vitest'

import { endOpenReasoning, mapUpdate, newTurnState } from './acp-map-update'

import type { SessionUpdate } from '@agentclientprotocol/sdk'

function thought(text: string): SessionUpdate {
  return {
    sessionUpdate: 'agent_thought_chunk',
    content: { type: 'text', text }
  } as unknown as SessionUpdate
}

function toolCall(): SessionUpdate {
  return {
    sessionUpdate: 'tool_call',
    toolCallId: 'tc1',
    title: 'list_requests'
  } as unknown as SessionUpdate
}

describe('mapUpdate reasoning runs', () => {
  it('연속되는 thought 델타는 하나의 reasoning 부품으로 합친다', () => {
    const state = newTurnState()
    const first = mapUpdate(thought('먼저 '), 'text-1', state)
    const second = mapUpdate(thought('카탈로그를'), 'text-1', state)

    expect(first.map((c) => c.type)).toEqual(['reasoning-start', 'reasoning-delta'])
    expect(second.map((c) => c.type)).toEqual(['reasoning-delta'])
    expect(state.reasoningStarted).toBe(true)
    expect(state.reasoningRuns).toBe(1)
  })

  it('thought 뒤 tool_call이 오면 열려 있던 reasoning 부품을 닫는다', () => {
    const state = newTurnState()
    mapUpdate(thought('생각 중'), 'text-1', state)
    const chunks = mapUpdate(toolCall(), 'text-1', state)

    expect(chunks[0]).toMatchObject({ type: 'reasoning-end' })
    expect(state.reasoningStarted).toBe(false)
  })

  it('tool_call 뒤 새 thought는 새 run id로 부품을 연다', () => {
    const state = newTurnState()
    mapUpdate(thought('첫 생각'), 'text-1', state)
    mapUpdate(toolCall(), 'text-1', state)
    const reopened = mapUpdate(thought('다음 생각'), 'text-1', state)

    const start = reopened.find((c) => c.type === 'reasoning-start')
    expect(start).toMatchObject({ id: 'reasoning-text-1-2' })
    expect(state.reasoningRuns).toBe(2)
  })

  it('endOpenReasoning은 열려 있는 부품만 닫는다', () => {
    const state = newTurnState()
    expect(endOpenReasoning('text-1', state)).toEqual([])

    mapUpdate(thought('생각'), 'text-1', state)
    expect(endOpenReasoning('text-1', state)).toEqual([
      { type: 'reasoning-end', id: 'reasoning-text-1-1' }
    ])
    expect(endOpenReasoning('text-1', state)).toEqual([])
  })
})
