import { create } from 'zustand'

import type { ViewportMode } from '../../../preload'

interface ViewportState {
  mode: ViewportMode
  setMode: (m: ViewportMode) => void
}

export const useViewportStore = create<ViewportState>((set) => ({
  mode: 'desktop',
  setMode: (m) => set({ mode: m })
}))
