import { describe, it, expect } from 'vitest'

import { runSequence, type WsStep } from './ws-sequence'

const step = (payload: string, delayMs = 0): WsStep => ({ payload, delayMs })

describe('runSequence', () => {
  it('순서대로 보내고, 각 프레임 앞에서 지정한 시간만큼 기다린다', async () => {
    const log: string[] = []
    const r = await runSequence(
      [step('auth', 0), step('subscribe', 100)],
      async (s) => (log.push(`send:${s.payload}`), { ok: true }),
      async (ms) => void log.push(`wait:${ms}`)
    )
    expect(r).toEqual({ sent: 2 })
    expect(log).toEqual(['send:auth', 'wait:100', 'send:subscribe'])
  })

  it('첫 프레임만 재연결을 허용한다', async () => {
    const flags: boolean[] = []
    await runSequence([step('a'), step('b'), step('c')], async (_s, reconnect) => (flags.push(reconnect), { ok: true }))
    expect(flags).toEqual([true, false, false])
  })

  it('실패한 단계에서 멈추고 몇 개를 보냈는지와 사유를 돌려준다', async () => {
    const sent: string[] = []
    const r = await runSequence([step('a'), step('b'), step('c')], async (s) => {
      if (s.payload === 'b') return { ok: false, error: 'the socket is no longer open' }
      sent.push(s.payload)
      return { ok: true }
    })
    expect(r).toEqual({ sent: 1, error: 'the socket is no longer open' })
    expect(sent).toEqual(['a'])
  })
})
