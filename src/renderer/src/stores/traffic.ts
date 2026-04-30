import { create } from 'zustand'

import type { NetworkEvent, TrafficEntry } from '@/types/traffic'

interface TrafficState {
  entries: Record<string, TrafficEntry>
  order: string[]
  selected: Set<string>
  detailId: string | null
  lastSelectedId: string | null
  applyEvent: (event: NetworkEvent) => void
  clear: () => void
  toggleSelect: (id: string) => void
  selectRange: (toId: string) => void
  clearSelection: () => void
  openDetail: (id: string) => void
  closeDetail: () => void
}

export const useTrafficStore = create<TrafficState>((set, get) => ({
  entries: {},
  order: [],
  selected: new Set<string>(),
  detailId: null,
  lastSelectedId: null,
  applyEvent: (event) =>
    set((s) => {
      const id = event.request_id
      const existing = s.entries[id]
      let next: TrafficEntry
      let order = s.order

      if (event.type === 'request') {
        if (existing) return s
        next = {
          requestId: id,
          url: event.url,
          method: event.method,
          resourceType: event.resource_type,
          startedAt: event.timestamp
        }
        order = [...s.order, id]
      } else if (event.type === 'response') {
        if (!existing) return s
        next = {
          ...existing,
          status: event.status,
          mimeType: event.mime_type
        }
      } else {
        if (!existing) return s
        next = {
          ...existing,
          encodedDataLength: event.encoded_data_length,
          completedAt: event.timestamp
        }
      }

      return {
        entries: { ...s.entries, [id]: next },
        order
      }
    }),
  clear: () =>
    set({ entries: {}, order: [], selected: new Set(), detailId: null, lastSelectedId: null }),
  toggleSelect: (id) =>
    set((s) => {
      const next = new Set(s.selected)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return { selected: next, lastSelectedId: id }
    }),
  selectRange: (toId) => {
    const { order, lastSelectedId, selected } = get()
    if (!lastSelectedId) {
      set({ selected: new Set([toId]), lastSelectedId: toId })
      return
    }
    const fromIdx = order.indexOf(lastSelectedId)
    const toIdx = order.indexOf(toId)
    if (fromIdx < 0 || toIdx < 0) return
    const [a, b] = fromIdx <= toIdx ? [fromIdx, toIdx] : [toIdx, fromIdx]
    const next = new Set(selected)
    for (let i = a; i <= b; i++) next.add(order[i])
    set({ selected: next, lastSelectedId: toId })
  },
  clearSelection: () => set({ selected: new Set(), lastSelectedId: null }),
  openDetail: (id) => set({ detailId: id }),
  closeDetail: () => set({ detailId: null })
}))
