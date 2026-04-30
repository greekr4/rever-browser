import { create } from 'zustand'

interface ChatDraftState {
  pending: string | null
  push: (text: string) => void
  consume: () => string | null
}

export const useChatDraft = create<ChatDraftState>((set, get) => ({
  pending: null,
  push: (text) => set({ pending: text }),
  consume: () => {
    const v = get().pending
    if (v != null) set({ pending: null })
    return v
  }
}))
