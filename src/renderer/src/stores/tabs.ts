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
  // Isolated session partition (proxy or non-default profile tabs). Undefined =
  // the shared persist:rever-shared partition. Set once at creation — a
  // webview's partition cannot change after mount.
  partition?: string
  // Which named profile this tab browses under. Undefined / 'default' = the
  // Default (shared) profile. Used for the tab-bar label.
  profileId?: string
}

interface TabsState {
  tabs: Tab[]
  activeId: string

  addTab: (
    url: string,
    opts?: {
      activate?: boolean
      proxy?: ProxyConfig
      partition?: string
      profileId?: string
    }
  ) => string
  closeTab: (id: string) => void
  reopenTab: () => void
  selectTab: (id: string) => void
  updateTab: (id: string, patch: Partial<Omit<Tab, 'id' | 'initialUrl'>>) => void
  setTabProxy: (id: string, proxy: ProxyConfig | undefined) => void
}

const INITIAL_URL = 'https://github.com/greekr4/rever-browser'

let nextId = 1
const newId = (): string => `t${nextId++}`

// URLs of recently closed tabs, newest last — backs Cmd/Ctrl+Shift+T.
const closedUrls: string[] = []

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
      initialUrl: url,
      proxy: opts?.proxy,
      partition: opts?.partition,
      profileId: opts?.profileId
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
    closedUrls.push(tabs[idx].url)
    const next = tabs.filter((t) => t.id !== id)
    let nextActive = activeId
    if (activeId === id) {
      const neighbour = next[idx] ?? next[idx - 1] ?? next[0]
      nextActive = neighbour.id
    }
    set({ tabs: next, activeId: nextActive })
  },

  reopenTab: () => {
    const url = closedUrls.pop()
    if (url) get().addTab(url)
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
