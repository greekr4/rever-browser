import { create } from 'zustand'

import type { ApiEndpoint, NetworkEvent, TrafficEntry } from '@/types/traffic'

import { templatePath } from '../../../shared/endpoint-template'

const MAX_ENTRIES = 500
// Endpoints are aggregated once per request (O(1)), never by rescanning entries.
// They outlive the entry ring buffer on purpose — the map should not forget an
// endpoint just because its request scrolled out.
const MAX_ENDPOINTS = 300
const API_TYPES = new Set(['XHR', 'Fetch'])
const AUTH_HINT = /(oauth|token|login|signin|auth|session|refresh)/i

function aggregate(
  current: Record<string, ApiEndpoint>,
  entry: TrafficEntry,
  ts: number
): Record<string, ApiEndpoint> {
  let u: URL
  try {
    u = new URL(entry.url)
  } catch {
    return current
  }
  const path = templatePath(u.pathname)
  const key = `${entry.method} ${u.origin}${path}`
  entry.endpointKey = key

  const hit = current[key]
  const next = {
    ...current,
    [key]: hit
      ? { ...hit, count: hit.count + 1, lastRequestId: entry.requestId, lastSeen: ts }
      : {
          key,
          origin: u.origin,
          host: u.hostname,
          path,
          method: entry.method,
          count: 1,
          lastRequestId: entry.requestId,
          lastSeen: ts,
          statuses: [],
          auth: AUTH_HINT.test(path)
        }
  }

  const keys = Object.keys(next)
  if (keys.length > MAX_ENDPOINTS) {
    // drop the least-recently-seen endpoints
    keys
      .sort((a, b) => next[a].lastSeen - next[b].lastSeen)
      .slice(0, keys.length - MAX_ENDPOINTS)
      .forEach((k) => delete next[k])
  }
  return next
}

interface TrafficState {
  entries: Record<string, TrafficEntry>
  order: string[]
  endpoints: Record<string, ApiEndpoint>
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
  endpoints: {},
  selected: new Set<string>(),
  detailId: null,
  lastSelectedId: null,
  applyEvent: (event) =>
    set((s) => {
      const id = event.request_id
      const existing = s.entries[id]
      let next: TrafficEntry
      let order = s.order
      let endpoints = s.endpoints

      if (event.type === 'request') {
        if (existing) return s
        next = {
          requestId: id,
          url: event.url,
          method: event.method,
          resourceType: event.resource_type,
          startedAt: event.timestamp
        }
        if (API_TYPES.has(event.resource_type)) {
          endpoints = aggregate(s.endpoints, next, event.timestamp)
        }
        order = [...s.order, id]
        // 상한 초과 시 오래된 항목 제거
        if (order.length > MAX_ENTRIES) {
          const removed = order.splice(0, order.length - MAX_ENTRIES)
          const nextEntries = { ...s.entries, [id]: next }
          for (const rid of removed) delete nextEntries[rid]
          return { entries: nextEntries, order, endpoints }
        }
      } else if (event.type === 'response') {
        if (!existing) return s
        next = {
          ...existing,
          status: event.status,
          mimeType: event.mime_type
        }
        const bucket = existing.endpointKey ? s.endpoints[existing.endpointKey] : undefined
        if (bucket && !bucket.statuses.includes(event.status)) {
          endpoints = {
            ...s.endpoints,
            [bucket.key]: { ...bucket, statuses: [...bucket.statuses, event.status] }
          }
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
        order,
        endpoints
      }
    }),
  clear: () =>
    set({
      entries: {},
      order: [],
      endpoints: {},
      selected: new Set(),
      detailId: null,
      lastSelectedId: null
    }),
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
