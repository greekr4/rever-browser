import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'

import { useNotifyStore, notifyEnabled } from './notify'

// The test env has no DOM, so back localStorage with an in-memory map.
beforeEach(() => {
  const store: Record<string, string> = {}
  vi.stubGlobal('localStorage', {
    getItem: (k: string) => store[k] ?? null,
    setItem: (k: string, v: string) => {
      store[k] = v
    },
    removeItem: (k: string) => {
      delete store[k]
    },
    clear: () => {
      for (const k of Object.keys(store)) delete store[k]
    }
  })
  useNotifyStore.setState({ enabled: true })
})

afterEach(() => vi.unstubAllGlobals())

describe('useNotifyStore 알림 설정', () => {
  it('기본값은 켜짐이다', () => {
    expect(notifyEnabled()).toBe(true)
  })

  it('toggle 이 상태를 뒤집고 localStorage 에 저장한다', () => {
    useNotifyStore.getState().toggle()
    expect(notifyEnabled()).toBe(false)
    expect(localStorage.getItem('rev:notify')).toBe('off')

    useNotifyStore.getState().toggle()
    expect(notifyEnabled()).toBe(true)
    expect(localStorage.getItem('rev:notify')).toBe('on')
  })
})
