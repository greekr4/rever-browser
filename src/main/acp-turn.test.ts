import { describe, expect, it } from 'vitest'
import { isStaleEmptyTurn } from './acp-turn'

describe('isStaleEmptyTurn', () => {
  it('직전 턴이 실패했고 업데이트 없이 즉시 end_turn이면 stale로 판정한다', () => {
    expect(
      isStaleEmptyTurn({ previousPromptFailed: true, updateCount: 0, stopReason: 'end_turn', elapsedMs: 3 })
    ).toBe(true)
  })
  it('직전 턴이 성공했으면 stale이 아니다', () => {
    expect(
      isStaleEmptyTurn({ previousPromptFailed: false, updateCount: 0, stopReason: 'end_turn', elapsedMs: 3 })
    ).toBe(false)
  })
  it('업데이트가 하나라도 있으면 stale이 아니다', () => {
    expect(
      isStaleEmptyTurn({ previousPromptFailed: true, updateCount: 1, stopReason: 'end_turn', elapsedMs: 3 })
    ).toBe(false)
  })
  it('cancelled 등 다른 stopReason이면 stale이 아니다', () => {
    expect(
      isStaleEmptyTurn({ previousPromptFailed: true, updateCount: 0, stopReason: 'cancelled', elapsedMs: 3 })
    ).toBe(false)
  })
  it('1초 이상 걸린 빈 턴은 stale로 보지 않는다', () => {
    expect(
      isStaleEmptyTurn({ previousPromptFailed: true, updateCount: 0, stopReason: 'end_turn', elapsedMs: 2500 })
    ).toBe(false)
  })
})
