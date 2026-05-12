import { create } from 'zustand'

import type { RepeaterRequestSpec, RepeaterResponse } from '../../../preload'

export type { RepeaterRequestSpec, RepeaterResponse }

interface HistoryEntry {
  request: RepeaterRequestSpec
  response: RepeaterResponse
  ts: number
}

interface RepeaterState {
  active: RepeaterRequestSpec | null
  sourceRequestId: string | null
  sourceLabel: string | null
  history: HistoryEntry[]
  loading: boolean
  error: string | null
  loadFromTraffic: (requestId: string) => Promise<void>
  setActive: (req: RepeaterRequestSpec) => void
  send: () => Promise<void>
  restoreFromHistory: (idx: number) => void
  clear: () => void
}

const MAX_HISTORY = 20

export const useRepeaterStore = create<RepeaterState>((set, get) => ({
  active: null,
  sourceRequestId: null,
  sourceLabel: null,
  history: [],
  loading: false,
  error: null,

  loadFromTraffic: async (requestId) => {
    const stored = await window.rev.traffic.get(requestId)
    if (!stored) {
      set({ error: 'request not found' })
      return
    }
    set({
      active: {
        url: stored.url,
        method: stored.method || 'GET',
        headers: { ...(stored.requestHeaders ?? {}) },
        body: stored.requestPostData ?? undefined
      },
      sourceRequestId: requestId,
      sourceLabel: `${stored.method} ${stored.url}`,
      history: [],
      error: null
    })
  },

  setActive: (req) => set({ active: req }),

  send: async () => {
    const { active } = get()
    if (!active) return
    set({ loading: true, error: null })
    try {
      const res = await window.rev.repeater.sendRaw(active)
      set((s) => ({
        history: [
          { request: cloneSpec(active), response: res, ts: Date.now() },
          ...s.history
        ].slice(0, MAX_HISTORY),
        loading: false,
        error: res.error ?? null
      }))
    } catch (e) {
      set({ loading: false, error: e instanceof Error ? e.message : String(e) })
    }
  },

  restoreFromHistory: (idx) => {
    const entry = get().history[idx]
    if (entry) set({ active: cloneSpec(entry.request) })
  },

  clear: () =>
    set({
      active: null,
      sourceRequestId: null,
      sourceLabel: null,
      history: [],
      error: null
    })
}))

function cloneSpec(s: RepeaterRequestSpec): RepeaterRequestSpec {
  return { url: s.url, method: s.method, headers: { ...s.headers }, body: s.body }
}
