import { create } from 'zustand'

import type { ACPAgentID } from '@/constants'

// The agent picked during first-run onboarding. ChatPanel seeds its default
// from `read()` at mount and subscribes to `choice` so a pick made in the
// onboarding modal switches the live chat without a reload.

const KEY = 'rev:agent-onboarded'

interface Stored {
  id: ACPAgentID
  path: string | null
}

/** Read the persisted pick. Returns null on first run or after a skip. */
export function readAgentChoice(): Stored | null {
  try {
    const raw = localStorage.getItem(KEY)
    if (!raw || raw === 'skipped') return null
    const parsed = JSON.parse(raw) as Stored
    return parsed?.id ? parsed : null
  } catch {
    return null
  }
}

/** True once the user has either picked an agent or skipped onboarding. */
export function hasOnboarded(): boolean {
  return localStorage.getItem(KEY) !== null
}

interface AgentChoiceState {
  /** Set only when the user picks during this session (null = untouched). */
  choice: Stored | null
  pick: (id: ACPAgentID, path: string | null) => void
  skip: () => void
}

export const useAgentChoice = create<AgentChoiceState>((set) => ({
  choice: null,
  pick: (id, path) => {
    localStorage.setItem(KEY, JSON.stringify({ id, path }))
    set({ choice: { id, path } })
  },
  skip: () => {
    localStorage.setItem(KEY, 'skipped')
  }
}))
