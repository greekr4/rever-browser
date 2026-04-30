import { create } from 'zustand'
import { persist } from 'zustand/middleware'

type BrowserMode = 'embedded' | 'external'

interface BrowserModeState {
  mode: BrowserMode
  setMode: (mode: BrowserMode) => void
}

export const useBrowserModeStore = create<BrowserModeState>()(
  persist(
    (set) => ({
      mode: 'embedded',
      setMode: (mode) => set({ mode })
    }),
    { name: 'rev:browser-mode' }
  )
)
