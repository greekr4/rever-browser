import { describe, expect, it } from 'vitest'

import { AGENT_CATALOG } from '../agent-catalog'
import { ACP_AGENTS, MODEL_CATALOG } from '../../renderer/src/constants'
import {
  GROK_DEFAULT_MODEL,
  GROK_MODELS,
  XAI_BASE_URL,
  isGrokSessionId
} from './grok-models'

describe('Grok catalog', () => {
  it('기본 모델은 grok-4.7이고 채팅 모델만 담는다', () => {
    expect(GROK_DEFAULT_MODEL).toBe('grok-4.7')
    expect(GROK_MODELS.map((m) => m.modelId)).toEqual([
      'grok-4.7',
      'grok-4.6',
      'grok-4.5',
      'grok-4.3'
    ])
    expect(GROK_MODELS.every((m) => !/imagine|voice|video/i.test(m.modelId))).toBe(true)
    expect(XAI_BASE_URL).toBe('https://api.x.ai/v1')
  })

  it('피커 카탈로그의 id와 이름이 메인 목록과 같다', () => {
    const picker = MODEL_CATALOG.filter((m) => m.agentId === 'grok')
    expect(picker.map((m) => ({ modelId: m.modelId, name: m.name }))).toEqual(
      GROK_MODELS.map((m) => ({ modelId: m.modelId, name: m.name }))
    )
  })

  it('렌더러와 메인 카탈로그가 키 기반 grok provider로 맞는다', () => {
    const ui = ACP_AGENTS.find((a) => a.id === 'grok')
    expect(ui?.provider).toBe('grok')
    expect(ui?.command).toBe('')
    expect(ui?.name).toBe('Grok (API)')
    expect(AGENT_CATALOG.find((a) => a.id === 'grok')).toEqual({
      id: 'grok',
      provider: 'grok',
      command: '',
      args: []
    })
  })

  it('grok 세션 id는 openai 세션 id와 겹치지 않는다', () => {
    expect(isGrokSessionId('grok:abc')).toBe(true)
    expect(isGrokSessionId('openai:abc')).toBe(false)
    expect(isGrokSessionId('anthropic:abc')).toBe(false)
  })
})
