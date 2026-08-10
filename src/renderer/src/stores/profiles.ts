import { create } from 'zustand'

import type { BrowserInfo, BrowserProfile, BrowserProfileInfo, ProfileKind } from '../../../preload'
import { useTabsStore } from './tabs'

export interface ImportProfileResult {
  ok: boolean
  imported: number
  total: number
  error?: string
}

// Named browsing profiles, mirrored from main via IPC. Persistent profiles
// survive restarts and back cookie import; incognito profiles are ephemeral.

const DEFAULT_PROFILE_ID = 'default'
const NEW_TAB_URL = 'https://www.google.com'

interface ProfilesState {
  profiles: BrowserProfile[]
  loaded: boolean
  load: () => Promise<void>
  create: (name: string, kind: ProfileKind, source?: string) => Promise<BrowserProfile>
  remove: (id: string) => Promise<void>
  // Open a new tab that browses under the given profile.
  openTab: (profile: BrowserProfile) => Promise<void>
  // Create a persistent profile named after a real browser profile, seeded with
  // that profile's cookies (all hosts), then open a tab in it.
  importFromBrowser: (
    browser: BrowserInfo,
    profile: BrowserProfileInfo
  ) => Promise<ImportProfileResult>
}

export const useProfilesStore = create<ProfilesState>((set, get) => ({
  profiles: [],
  loaded: false,

  load: async () => {
    const profiles = await window.rev.profiles.list()
    set({ profiles, loaded: true })
  },

  create: async (name, kind, source) => {
    const profile = await window.rev.profiles.create(name, kind, source)
    set((s) => ({ profiles: [...s.profiles, profile] }))
    return profile
  },

  remove: async (id) => {
    if (id === DEFAULT_PROFILE_ID) return
    await window.rev.profiles.delete(id)
    set((s) => ({ profiles: s.profiles.filter((p) => p.id !== id) }))
  },

  openTab: async (profile) => {
    const isDefault = profile.id === DEFAULT_PROFILE_ID
    const id = useTabsStore.getState().addTab(NEW_TAB_URL, {
      partition: isDefault ? undefined : profile.partition,
      profileId: profile.id
    })
    if (isDefault) return
    // Register the partition so cookie import / current-ip target this profile,
    // then re-assert the active tab (creation may have fired before this call).
    await window.rev.profiles.registerTabPartition(id, profile.partition)
    await window.rev.proxy.setActiveTab(id)
  },

  importFromBrowser: async (browser, srcProfile) => {
    // New persistent profile named after the source browser profile, sourced
    // from the browser (e.g. name "태균", source "Chrome").
    const profile = await get().create(srcProfile.name, 'persistent', browser.name)
    // openTab makes this profile's partition the active session, so the import
    // below lands in it.
    await get().openTab(profile)
    const r = await window.rev.storage.browserImport({
      browser: browser.id,
      profile: srcProfile.id,
      hosts: []
    })
    return { ok: r.ok, imported: r.imported, total: r.total, error: r.error }
  }
}))
