import { useEffect } from 'react'

import { useAppThemeStore, resolveTheme } from '@/stores/app-theme'

// Applies the resolved app theme to <html data-theme>, paints the document
// background, and keeps the native titlebar overlay in sync. Re-runs on manual
// mode change and on OS scheme changes while in 'system' mode. Returns the
// current mode and a cycle() for the toolbar toggle.
export function useAppTheme(): { themeMode: ReturnType<typeof useAppThemeStore.getState>['mode']; cycleAppTheme: () => void } {
  const themeMode = useAppThemeStore((s) => s.mode)
  const cycleAppTheme = useAppThemeStore((s) => s.cycle)

  useEffect(() => {
    const apply = (): void => {
      const resolved = resolveTheme(themeMode)
      document.documentElement.setAttribute('data-theme', resolved)
      document.documentElement.style.background = resolved === 'dark' ? '#0e0e0e' : '#fbfbfc'
      void window.rev.theme.setTitlebar(resolved)
    }
    apply()
    const mq = window.matchMedia('(prefers-color-scheme: dark)')
    const onChange = (): void => {
      if (useAppThemeStore.getState().mode === 'system') apply()
    }
    mq.addEventListener('change', onChange)
    return () => mq.removeEventListener('change', onChange)
  }, [themeMode])

  return { themeMode, cycleAppTheme }
}
