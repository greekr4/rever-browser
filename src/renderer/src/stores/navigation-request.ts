import { create } from 'zustand'

interface PendingNavigation {
  url: string
  ts: number
}

interface State {
  pending: PendingNavigation | null
  request: (url: string) => void
  clear: () => void
}

export const useNavigationRequestStore = create<State>((set) => ({
  pending: null,
  request: (url) => set({ pending: { url, ts: Date.now() } }),
  clear: () => set({ pending: null })
}))
