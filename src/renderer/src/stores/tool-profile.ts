import { create } from 'zustand'

export type ToolProfileId = 'browse' | 'reverse' | 'probe'

interface ToolProfileState {
  profile: ToolProfileId
  setProfile: (profile: ToolProfileId) => Promise<void>
}

function isProfile(value: unknown): value is ToolProfileId {
  return value === 'browse' || value === 'reverse' || value === 'probe'
}

// Mirror of the main-process profile. Not persisted: main owns the file.
export const useToolProfileStore = create<ToolProfileState>((set, get) => ({
  profile: 'browse',
  setProfile: async (profile) => {
    const prev = get().profile
    set({ profile })
    try {
      await window.rev.toolProfile.set(profile)
    } catch (e) {
      if (get().profile === profile) set({ profile: prev })
      console.warn('[tool-profile] set failed:', e)
    }
  }
}))

if (typeof window !== 'undefined' && window.rev?.toolProfile) {
  void window.rev.toolProfile.get().then((profile) => {
    if (isProfile(profile)) useToolProfileStore.setState({ profile })
  })
  window.rev.toolProfile.onChange((profile) => {
    if (isProfile(profile)) useToolProfileStore.setState({ profile })
  })
}
