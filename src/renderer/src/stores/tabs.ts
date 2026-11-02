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

// Session restore: remember the open tabs across restarts so the app reopens
// where the user left off instead of always jumping to the home page. Proxy
// tabs are left out — their proxy config lives only in memory, so restoring the
// URL without it would load the page directly (wrong). Isolated-session
// partitions for named profiles are stable across restarts and kept.
const STORE_KEY = 'rev:tabs'

export interface SavedTab {
  url: string
  title?: string
  profileId?: string
  partition?: string
}

export function serializeTabs(tabs: Tab[], activeId: string): string {
  const savable = tabs.filter((t) => !t.proxy && t.url)
  const activeIndex = Math.max(
    0,
    savable.findIndex((t) => t.id === activeId)
  )
  return JSON.stringify({
    tabs: savable.map((t) => ({ url: t.url, title: t.title, profileId: t.profileId, partition: t.partition })),
    activeIndex
  })
}

export function parseSavedTabs(raw: string | null): { tabs: SavedTab[]; activeIndex: number } | null {
  if (!raw) return null
  try {
    const s = JSON.parse(raw) as { tabs?: unknown; activeIndex?: unknown }
    if (Array.isArray(s.tabs) && s.tabs.length > 0 && s.tabs.every((t) => typeof (t as SavedTab)?.url === 'string')) {
      return { tabs: s.tabs as SavedTab[], activeIndex: typeof s.activeIndex === 'number' ? s.activeIndex : 0 }
    }
  } catch {}
  return null
}

function loadInitial(): { tabs: Tab[]; activeId: string } {
  const saved = typeof localStorage !== 'undefined' ? parseSavedTabs(localStorage.getItem(STORE_KEY)) : null
  if (saved) {
    const tabs: Tab[] = saved.tabs.map((t) => ({
      id: newId(),
      url: t.url,
      title: t.title || 'New Tab',
      webContentsId: null,
      initialUrl: t.url,
      profileId: t.profileId,
      partition: t.partition
    }))
    const active = tabs[Math.min(Math.max(saved.activeIndex, 0), tabs.length - 1)]
    return { tabs, activeId: active.id }
  }
  const id = newId()
  return {
    tabs: [{ id, url: INITIAL_URL, title: 'New Tab', webContentsId: null, initialUrl: INITIAL_URL }],
    activeId: id
  }
}

const initial = loadInitial()

export const useTabsStore = create<TabsState>((set, get) => ({
  tabs: initial.tabs,
  activeId: initial.activeId,

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

// Persist the open tabs on every change so a restart reopens them.
if (typeof localStorage !== 'undefined') {
  useTabsStore.subscribe((s) => {
    try {
      localStorage.setItem(STORE_KEY, serializeTabs(s.tabs, s.activeId))
    } catch {}
  })
}
