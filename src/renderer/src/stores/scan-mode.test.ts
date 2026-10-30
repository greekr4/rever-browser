import { describe, it, expect, beforeEach, vi } from 'vitest'

vi.mock('./tool-profile', () => ({
  useToolProfileStore: { getState: () => ({ setProfile: () => Promise.resolve() }) }
}))

import { useScanModeStore } from './scan-mode'

describe('useScanModeStore (one-shot latch)', () => {
  beforeEach(() => useScanModeStore.setState({ armed: false }))

  it('기본은 무장 해제', () => {
    expect(useScanModeStore.getState().consume()).toBe(false)
  })

  it('arm 후 consume는 한 번만 true', () => {
    useScanModeStore.getState().arm()
    expect(useScanModeStore.getState().consume()).toBe(true)
    expect(useScanModeStore.getState().consume()).toBe(false)
  })
})
