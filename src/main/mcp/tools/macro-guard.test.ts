import { describe, expect, it } from 'vitest'

import {
  findForbiddenMacroStep,
  macroRunNeedsGuard,
  macroRunRefusal,
  screenMacroRun,
  type MacroStep
} from './macro-guard'

const noopResolve = async (): Promise<MacroStep[]> => []

describe('macro guard', () => {
  it('full 카탈로그는 검사하지 않고 follow와 direct만 검사한다', () => {
    expect(macroRunNeedsGuard('full')).toBe(false)
    expect(macroRunNeedsGuard('follow')).toBe(true)
    expect(macroRunNeedsGuard('direct')).toBe(true)
  })

  it('중첩 run_macro가 intruder_run이면 그 스텝을 거부하고 실행 경로를 열지 않는다', async () => {
    const steps: MacroStep[] = [{ tool: 'run_macro', input: { id: 'A' } }]
    const resolve = async (id: string): Promise<MacroStep[]> => {
      expect(id).toBe('A')
      return [{ tool: 'intruder_run' }]
    }
    const hit = await findForbiddenMacroStep(steps, resolve, ['B'])
    expect(hit).toEqual({ index: 0, tool: 'intruder_run' })
    const screen = await screenMacroRun(steps, resolve, ['B'])
    expect(screen).toEqual({ ok: false, error: macroRunRefusal(0, 'intruder_run') })
  })

  it('안쪽이 browser_snapshot뿐이면 null이라 실행으로 진행한다', async () => {
    const steps: MacroStep[] = [{ tool: 'run_macro', input: { id: 'A' } }]
    const resolve = async (): Promise<MacroStep[]> => [{ tool: 'browser_snapshot' }]
    expect(await findForbiddenMacroStep(steps, resolve, ['B'])).toBeNull()
    expect(await screenMacroRun(steps, resolve, ['B'])).toEqual({ ok: true })
  })

  it('메서드가 없거나 GET/HEAD가 아닌 replay는 거부하고 GET은 통과한다', async () => {
    expect(
      await findForbiddenMacroStep([{ tool: 'replay_request' }], noopResolve, [])
    ).toEqual({ index: 0, tool: 'replay_request' })
    expect(
      await findForbiddenMacroStep(
        [{ tool: 'repeater_send', input: { method: 'post' } }],
        noopResolve,
        []
      )
    ).toEqual({ index: 0, tool: 'repeater_send' })
    expect(
      await findForbiddenMacroStep(
        [{ tool: 'replay_request', input: { method: 'GET' } }],
        noopResolve,
        []
      )
    ).toBeNull()
    expect(
      await findForbiddenMacroStep(
        [{ tool: 'repeater_send', input: { modifications: { method: 'head' } } }],
        noopResolve,
        []
      )
    ).toBeNull()
  })

  it('이름 일부만 겹치는 도구와 순환, 없는 id는 각각 통과하거나 run_macro로 거부한다', async () => {
    expect(
      await findForbiddenMacroStep([{ tool: 'not_intruder_run_helper' }], noopResolve, [])
    ).toBeNull()
    expect(
      await findForbiddenMacroStep([{ tool: 'run_macro', input: { id: 'A' } }], noopResolve, ['A'])
    ).toEqual({ index: 0, tool: 'run_macro' })
    expect(
      await findForbiddenMacroStep([{ tool: 'run_macro', input: {} }], noopResolve, ['B'])
    ).toEqual({ index: 0, tool: 'run_macro' })
  })

  it('resolve가 던지면 screenMacroRun도 던지고 실행으로 넘어가지 않는다', async () => {
    const resolve = async (): Promise<MacroStep[]> => {
      throw new Error('missing macro')
    }
    await expect(
      screenMacroRun([{ tool: 'run_macro', input: { id: 'A' } }], resolve, ['B'])
    ).rejects.toThrow('missing macro')
  })
})
