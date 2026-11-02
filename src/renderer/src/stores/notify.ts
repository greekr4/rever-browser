import { create } from 'zustand'

// Whether the app shows OS notifications (e.g. when a Red Team scan finishes).
// Persisted so the choice survives restarts; defaults to on.
const KEY = 'rev:notify'

function load(): boolean {
  try {
    return localStorage.getItem(KEY) !== 'off'
  } catch {
    return true
  }
}

function save(v: boolean): void {
  try {
    localStorage.setItem(KEY, v ? 'on' : 'off')
  } catch {}
}

interface NotifyState {
  enabled: boolean
  setEnabled: (v: boolean) => void
  toggle: () => void
}

export const useNotifyStore = create<NotifyState>((set, get) => ({
  enabled: load(),
  setEnabled: (v) => {
    save(v)
    set({ enabled: v })
  },
  toggle: () => get().setEnabled(!get().enabled)
}))

// Non-hook read for code outside React (scan.ts fires notifications).
export function notifyEnabled(): boolean {
  return useNotifyStore.getState().enabled
}
