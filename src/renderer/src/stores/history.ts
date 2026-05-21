import { create } from 'zustand'
import { persist } from 'zustand/middleware'

export interface HistoryEntry {
  id: string
  tabId: string
  url: string
  title: string
  visitedAt: number
}

interface HistoryState {
  entries: HistoryEntry[]
  push: (entry: Omit<HistoryEntry, 'id' | 'visitedAt'>) => void
  updateTitle: (tabId: string, url: string, title: string) => void
  clear: () => void
}

const MAX_ENTRIES = 1000

function newId(): string {
  return `h${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`
}

export const useHistoryStore = create<HistoryState>()(
  persist(
    (set) => ({
      entries: [],
      push: (entry) =>
        set((s) => {
          const last = s.entries[s.entries.length - 1]
          if (last && last.tabId === entry.tabId && last.url === entry.url) {
            return s
          }
          const next: HistoryEntry = {
            ...entry,
            id: newId(),
            visitedAt: Date.now()
          }
          const entries = [...s.entries, next]
          if (entries.length > MAX_ENTRIES) entries.splice(0, entries.length - MAX_ENTRIES)
          return { entries }
        }),
      updateTitle: (tabId, url, title) =>
        set((s) => {
          for (let i = s.entries.length - 1; i >= 0; i--) {
            const e = s.entries[i]
            if (e.tabId === tabId && e.url === url) {
              if (e.title === title) return s
              const entries = s.entries.slice()
              entries[i] = { ...e, title }
              return { entries }
            }
          }
          return s
        }),
      clear: () => set({ entries: [] })
    }),
    {
      name: 'rev:history',
      partialize: (s) => ({ entries: s.entries })
    }
  )
)
