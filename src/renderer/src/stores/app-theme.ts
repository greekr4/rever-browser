import { create } from 'zustand'
import { persist } from 'zustand/middleware'

// App-level UI theme. Distinct from the per-origin *webview* theme
// (webview-theme.ts) which recolors the loaded site — this themes the
// rever-browser chrome itself. 'system' follows the OS prefers-color-scheme.
export type ThemeMode = 'system' | 'light' | 'dark'
export type ResolvedTheme = 'light' | 'dark'

interface AppThemeState {
  mode: ThemeMode
  setMode: (mode: ThemeMode) => void
  /** Cycle System → Light → Dark → System. */
  cycle: () => void
}

const ORDER: ThemeMode[] = ['system', 'light', 'dark']

// localStorage key must match the inline bootstrap script in index.html that
// applies data-theme before first paint (prevents a theme flash on launch).
export const APP_THEME_STORAGE_KEY = 'rev:app-theme'

export const useAppThemeStore = create<AppThemeState>()(
  persist(
    (set, get) => ({
      mode: 'system',
      setMode: (mode) => set({ mode }),
      cycle: () => {
        const idx = ORDER.indexOf(get().mode)
        set({ mode: ORDER[(idx + 1) % ORDER.length] })
      }
    }),
    { name: APP_THEME_STORAGE_KEY }
  )
)

export function systemPrefersDark(): boolean {
  return (
    typeof window !== 'undefined' &&
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-color-scheme: dark)').matches
  )
}

export function resolveTheme(mode: ThemeMode): ResolvedTheme {
  if (mode === 'system') return systemPrefersDark() ? 'dark' : 'light'
  return mode
}
