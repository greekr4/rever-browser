import { create } from 'zustand'

import type { NetworkEvent, TrafficEntry } from '@/types/traffic'

interface TrafficState {
  entries: Record<string, TrafficEntry>
  order: string[]
  applyEvent: (event: NetworkEvent) => void
  clear: () => void
}

export const useTrafficStore = create<TrafficState>((set) => ({
  entries: {},
  order: [],
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
  clear: () => set({ entries: {}, order: [] })
}))

export const selectTrafficList = (s: TrafficState): TrafficEntry[] =>
  s.order.map((id) => s.entries[id]).filter(Boolean)
