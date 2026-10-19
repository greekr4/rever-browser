import { describe, expect, it } from 'vitest'

import { beginDirectTurn, directToolRefusal, takeDirectTool } from './direct-tool-gate'

describe('direct tool gate', () => {
  it('browse에서 빈 dispatched로 intruder_run을 두 번 거부하고 다음 목록에 넣지 않는다', () => {
    const dispatched = new Set<string>()
    const allowed = beginDirectTurn('browse', dispatched)
    const first = takeDirectTool('intruder_run', allowed, dispatched)
    expect(first).toEqual({ run: false, content: directToolRefusal('intruder_run') })
    expect([...dispatched]).toEqual([])

    const next = beginDirectTurn('browse', dispatched)
    expect(next.has('intruder_run')).toBe(false)
    const second = takeDirectTool('intruder_run', next, dispatched)
    expect(second).toEqual({ run: false, content: directToolRefusal('intruder_run') })
    expect([...dispatched]).toEqual([])
  })

  it('callTool에 넘긴 browser_snapshot은 다음 목록에 남는다', () => {
    const dispatched = new Set<string>()
    const allowed = beginDirectTurn('browse', dispatched)
    expect(takeDirectTool('browser_snapshot', allowed, dispatched)).toEqual({ run: true })
    expect(dispatched.has('browser_snapshot')).toBe(true)
    expect(beginDirectTurn('browse', dispatched).has('browser_snapshot')).toBe(true)
  })

  it('같은 배치의 allowed는 tool_profile_set 이후에도 바뀌지 않는다', () => {
    const dispatched = new Set<string>()
    const allowed = beginDirectTurn('browse', dispatched)
    expect(takeDirectTool('tool_profile_set', allowed, dispatched)).toEqual({ run: true })
    const intruder = takeDirectTool('intruder_run', allowed, dispatched)
    expect(intruder.run).toBe(false)
    expect([...dispatched]).toEqual(['tool_profile_set'])
    expect(beginDirectTurn('browse', dispatched).has('intruder_run')).toBe(false)
  })
})
