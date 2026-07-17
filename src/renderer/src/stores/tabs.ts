import { create } from 'zustand'

// Per-tab upstream proxy. Mirrors main/tab-proxy.ts TabProxyConfig. Held in
// memory only (tabs aren't persisted), so it resets on restart.
export interface ProxyConfig {
  enabled: boolean
  scheme: 'http' | 'https' | 'socks5'
  host: string
  port: number
  username?: string
  password?: string
}

export interface Tab {
  id: string
  url: string
  title: string
  webContentsId: number | null
  // Set when the tab is first created. Used as the webview's `src` exactly
  // once — afterwards the user/AI drives navigation via loadURL on the ref,
  // so React doesn't re-mount the webview.
  initialUrl: string
  // Undefined = direct connection (default).
  proxy?: ProxyConfig
}

interface TabsState {
  tabs: Tab[]
  activeId: string

  addTab: (url: string, opts?: { activate?: boolean }) => string
  closeTab: (id: string) => void
  selectTab: (id: string) => void
  updateTab: (id: string, patch: Partial<Omit<Tab, 'id' | 'initialUrl'>>) => void
  setTabProxy: (id: string, proxy: ProxyConfig | undefined) => void
}

const INITIAL_URL = 'https://www.google.com'

let nextId = 1
const newId = (): string => `t${nextId++}`

const firstId = newId()

export const useTabsStore = create<TabsState>((set, get) => ({
  tabs: [
    {
      id: firstId,
      url: INITIAL_URL,
      title: 'New Tab',
      webContentsId: null,
      initialUrl: INITIAL_URL
    }
  ],
  activeId: firstId,

  addTab: (url, opts) => {
    const id = newId()
    const tab: Tab = {
      id,
      url,
      title: 'New Tab',
      webContentsId: null,
      initialUrl: url
    }
    set((s) => ({
      tabs: [...s.tabs, tab],
      activeId: opts?.activate === false ? s.activeId : id
    }))
    return id
  },

  closeTab: (id) => {
    const { tabs, activeId } = get()
    if (tabs.length === 1) return // never close the last tab
    const idx = tabs.findIndex((t) => t.id === id)
    if (idx < 0) return
    const next = tabs.filter((t) => t.id !== id)
    let nextActive = activeId
    if (activeId === id) {
      const neighbour = next[idx] ?? next[idx - 1] ?? next[0]
      nextActive = neighbour.id
    }
    set({ tabs: next, activeId: nextActive })
  },

  selectTab: (id) => {
    if (!get().tabs.some((t) => t.id === id)) return
    set({ activeId: id })
  },

  updateTab: (id, patch) => {
    set((s) => ({
      tabs: s.tabs.map((t) => (t.id === id ? { ...t, ...patch } : t))
    }))
  },

  setTabProxy: (id, proxy) => {
    set((s) => ({
      tabs: s.tabs.map((t) => (t.id === id ? { ...t, proxy } : t))
    }))
  }
}))
