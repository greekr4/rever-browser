import { afterEach, describe, expect, it, vi } from 'vitest'

import { startTurnPulse } from './turn-pulse'

describe('startTurnPulse', () => {
  afterEach(() => {
    vi.useRealTimers()
  })

  it('간격마다 rever_pulse를 보내고 멈추면 더 보내지 않는다', () => {
    vi.useFakeTimers()
    const seen: string[] = []
    const stop = startTurnPulse((notification) => {
      seen.push((notification.update as { sessionUpdate: string }).sessionUpdate)
    }, 'grok:1', 15_000)

    vi.advanceTimersByTime(15_000)
    expect(seen).toEqual(['rever_pulse'])
    vi.advanceTimersByTime(30_000)
    expect(seen).toHaveLength(3)

    stop()
    vi.advanceTimersByTime(60_000)
    expect(seen).toHaveLength(3)
  })
})
