import { describe, it, expect, beforeEach } from 'vitest'

import { agentEnv } from './agent-env'

describe('agentEnv', () => {
  beforeEach(() => {
    delete process.env.ANTHROPIC_MODEL
    delete process.env.CLAUDECODE
    delete process.env.CLAUDE_CODE_SOMETHING
  })

  it('claude 계열 에이전트는 기본 모델을 claude-opus-4-8로 고정한다', () => {
    const env = agentEnv('claude-agent-acp')
    expect(env.ANTHROPIC_MODEL).toBe('claude-opus-4-8')
  })

  it('claude 계열이 아니면 모델을 고정하지 않는다', () => {
    const env = agentEnv('/usr/local/bin/codex-acp')
    expect(env.ANTHROPIC_MODEL).toBeUndefined()
  })

  it('CLAUDECODE / CLAUDE_CODE_* 세션 마커는 제거한다', () => {
    process.env.CLAUDECODE = '1'
    process.env.CLAUDE_CODE_SOMETHING = '1'
    const env = agentEnv('claude-agent-acp')
    expect(env.CLAUDECODE).toBeUndefined()
    expect(env.CLAUDE_CODE_SOMETHING).toBeUndefined()
  })
})
