import { create } from 'zustand'

import { useToolProfileStore } from './tool-profile'

// One-shot scan latch. The Scan bar arms it; the transport consumes it on the
// next first-turn send to prepend the recon persona exactly once — so a later,
// unrelated chat never silently becomes a scan.
interface ScanModeState {
  armed: boolean
  arm: () => void
  consume: () => boolean
}

export const useScanModeStore = create<ScanModeState>((set, get) => ({
  armed: false,
  arm: () => {
    set({ armed: true })
    // Scan needs list_endpoints/security_inspect/scan_secrets/finding_export,
    // which live in the reverse profile.
    void useToolProfileStore.getState().setProfile('reverse')
  },
  consume: () => {
    const was = get().armed
    if (was) set({ armed: false })
    return was
  }
}))
