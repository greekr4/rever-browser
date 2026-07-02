import { create } from 'zustand'
import { persist } from 'zustand/middleware'

import type { UIMessage } from 'ai'
import type { ACPAgentID } from '@/constants'

export interface ChatConversation {
  id: string
  title: string
  agentId: ACPAgentID
  messages: UIMessage[]
  createdAt: number
  updatedAt: number
}

interface ChatHistoryState {
  conversations: ChatConversation[]
  /** Insert or update a conversation, keeping the list newest-first. */
  save: (input: { id: string; agentId: ACPAgentID; messages: UIMessage[] }) => void
  remove: (id: string) => void
  clear: () => void
}

// Bundle-heavy tool outputs live in messages, so cap the count to keep the
// serialized store well under the ~5MB localStorage quota.
const MAX_CONVERSATIONS = 30

export function newConversationId(): string {
  return `c${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`
}

function deriveTitle(messages: UIMessage[]): string {
  const firstUser = messages.find((m) => m.role === 'user')
  const text = firstUser?.parts
    .filter((p): p is { type: 'text'; text: string } => p.type === 'text')
    .map((p) => p.text)
    .join(' ')
    .trim()
  if (!text) return 'New conversation'
  return text.length > 60 ? `${text.slice(0, 60)}…` : text
}

export const useChatHistory = create<ChatHistoryState>()(
  persist(
    (set) => ({
      conversations: [],
      save: ({ id, agentId, messages }) =>
        set((s) => {
          if (messages.length === 0) return s
          const now = Date.now()
          const existing = s.conversations.find((c) => c.id === id)
          const conv: ChatConversation = {
            id,
            agentId,
            messages,
            title: deriveTitle(messages),
            createdAt: existing?.createdAt ?? now,
            updatedAt: now
          }
          const rest = s.conversations.filter((c) => c.id !== id)
          const conversations = [conv, ...rest]
          if (conversations.length > MAX_CONVERSATIONS) conversations.length = MAX_CONVERSATIONS
          return { conversations }
        }),
      remove: (id) =>
        set((s) => ({ conversations: s.conversations.filter((c) => c.id !== id) })),
      clear: () => set({ conversations: [] })
    }),
    {
      name: 'rev:chat-history',
      partialize: (s) => ({ conversations: s.conversations })
    }
  )
)
