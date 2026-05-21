import { create } from 'zustand'
import { persist } from 'zustand/middleware'

export type WebviewTheme = 'auto' | 'light' | 'dark'

interface WebviewThemeState {
  byOrigin: Record<string, WebviewTheme>
  get: (origin: string) => WebviewTheme
  set: (origin: string, theme: WebviewTheme) => void
  cycle: (origin: string) => WebviewTheme
}

const ORDER: WebviewTheme[] = ['auto', 'light', 'dark']

export function originFromUrl(url: string): string | null {
  try {
    const u = new URL(url)
    if (!u.protocol.startsWith('http')) return null
    return u.origin
  } catch {
    return null
  }
}

export const useWebviewThemeStore = create<WebviewThemeState>()(
  persist(
    (set, get) => ({
      byOrigin: {},
      get: (origin) => get().byOrigin[origin] ?? 'auto',
      set: (origin, theme) =>
        set((s) => {
          const next = { ...s.byOrigin }
          if (theme === 'auto') delete next[origin]
          else next[origin] = theme
          return { byOrigin: next }
        }),
      cycle: (origin) => {
        const current = get().byOrigin[origin] ?? 'auto'
        const idx = ORDER.indexOf(current)
        const next = ORDER[(idx + 1) % ORDER.length]
        get().set(origin, next)
        return next
      }
    }),
    { name: 'rev:webview-theme' }
  )
)

export const LIGHT_CSS = `
:root { color-scheme: light !important; }
html { background-color: #ffffff !important; }
body { background-color: #ffffff !important; color: #111111 !important; }
`

export const DARK_CSS = `
:root { color-scheme: dark !important; }
html { background-color: #1a1a1a !important; }
body { background-color: #1a1a1a !important; color: #e6e6e6 !important; }
`

export function cssForTheme(theme: WebviewTheme): string | null {
  if (theme === 'light') return LIGHT_CSS
  if (theme === 'dark') return DARK_CSS
  return null
}
