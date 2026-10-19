import { describe, expect, it } from 'vitest'

import {
  applyFollowCatalog,
  catalogDisablesTools,
  type ToolToggle
} from './tool-profile'

function harness(connected: boolean) {
  let sends = 0
  const mcp = {
    connected,
    isConnected() {
      return this.connected
    },
    sendToolListChanged() {
      sends += 1
    }
  }
  const handles = new Map<string, ToolToggle>()
  const add = (name: string, enabled: boolean) => {
    const handle: ToolToggle = {
      enabled,
      enable() {
        handle.enabled = true
        mcp.sendToolListChanged()
      },
      disable() {
        handle.enabled = false
        mcp.sendToolListChanged()
      }
    }
    handles.set(name, handle)
    return handle
  }
  return { mcp, handles, add, sends: () => sends }
}

describe('applyFollowCatalog', () => {
  it('연결된 서버에서 토글이 여러 번이어도 list_changed는 한 번이다', () => {
    const h = harness(true)
    h.add('browser_snapshot', true)
    h.add('grep_scripts', true)
    h.add('intruder_run', true)
    h.add('bp_add', true)

    applyFollowCatalog(h.mcp, h.handles, 'browse')

    expect(h.sends()).toBe(1)
    expect(h.handles.get('browser_snapshot')?.enabled).toBe(true)
    expect(h.handles.get('grep_scripts')?.enabled).toBe(false)
    expect(h.handles.get('intruder_run')?.enabled).toBe(false)
    expect(h.handles.get('bp_add')?.enabled).toBe(false)
    h.mcp.sendToolListChanged()
    expect(h.sends()).toBe(2)
  })

  it('비트가 그대로면 list_changed를 보내지 않는다', () => {
    const h = harness(true)
    h.add('browser_snapshot', true)
    h.add('grep_scripts', false)
    h.add('bp_add', false)

    applyFollowCatalog(h.mcp, h.handles, 'browse')

    expect(h.sends()).toBe(0)
    expect(h.handles.get('browser_snapshot')?.enabled).toBe(true)
    expect(h.handles.get('grep_scripts')?.enabled).toBe(false)
  })

  it('연결 전에는 비트가 바뀌어도 list_changed를 보내지 않는다', () => {
    const h = harness(false)
    const grep = h.add('grep_scripts', true)

    applyFollowCatalog(h.mcp, h.handles, 'browse')

    expect(h.sends()).toBe(0)
    expect(grep.enabled).toBe(false)
  })

  it('follow만 도구를 끄고 full과 direct는 끄지 않는다', () => {
    expect(catalogDisablesTools('follow')).toBe(true)
    expect(catalogDisablesTools('full')).toBe(false)
    expect(catalogDisablesTools('direct')).toBe(false)
  })
})
