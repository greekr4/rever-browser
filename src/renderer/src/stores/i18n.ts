import { create } from 'zustand'

import { en, type TKey } from '@/locales/en'
import { ko } from '@/locales/ko'

// Lightweight, dependency-free i18n. English is the source of truth; Korean
// falls back to English for any missing key. Add languages by adding a dict.

export type Lang = 'en' | 'ko'

const DICTS: Record<Lang, Partial<Record<TKey, string>>> = { en, ko }

const KEY = 'rev:lang'

interface I18nState {
  lang: Lang
  setLang: (lang: Lang) => void
}

export const useI18nStore = create<I18nState>((set) => ({
  lang: (localStorage.getItem(KEY) as Lang) || 'en',
  setLang: (lang) => {
    localStorage.setItem(KEY, lang)
    set({ lang })
  }
}))

export type TFn = (key: TKey, vars?: Record<string, string | number>) => string

// Hook: returns a `t` bound to the current language (re-renders on change).
export function useT(): TFn {
  const lang = useI18nStore((s) => s.lang)
  return (key, vars) => {
    let s = DICTS[lang][key] ?? en[key] ?? String(key)
    if (vars) for (const [k, v] of Object.entries(vars)) s = s.replaceAll(`{${k}}`, String(v))
    return s
  }
}
