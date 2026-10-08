import { create } from 'zustand'
import { persist } from 'zustand/middleware'

// A session audit trail of agent permission decisions, so a user can later
// trace which sensitive action was approved (or denied) and when. Mirrors the
// ARTEX guard/ audit-trail idea, scaled to a single-agent renderer store.
export interface ApprovalAuditEntry {
  id: string
  /** Best-effort tool label (matched risky tool id, else the request title). */
  tool: string
  /** Target host extracted from the tool input, if any. */
  target: string | null
  /** Whether the decision allowed or rejected the action. */
  decision: 'allow' | 'reject'
  /** True when resolved by auto-approve without showing the prompt. */
  auto: boolean
  /** True when this was a forced prompt for a risky tool. */
  risky: boolean
  ts: number
}

const MAX_ENTRIES = 200

interface AuditState {
  entries: ApprovalAuditEntry[]
  add: (entry: ApprovalAuditEntry) => void
  clear: () => void
}

export const useApprovalAuditStore = create<AuditState>()(
  persist(
    (set) => ({
      entries: [],
      add: (entry) => set((s) => ({ entries: [entry, ...s.entries].slice(0, MAX_ENTRIES) })),
      clear: () => set({ entries: [] })
    }),
    { name: 'rever-browser:approval-audit' }
  )
)

export const useApprovalAuditEntries = () => useApprovalAuditStore((s) => s.entries)

export function recordApprovalAudit(entry: Omit<ApprovalAuditEntry, 'id' | 'ts'>): void {
  useApprovalAuditStore.getState().add({
    ...entry,
    id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    ts: Date.now()
  })
}
