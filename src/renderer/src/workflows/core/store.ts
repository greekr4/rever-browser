import { create } from 'zustand'
import { persist } from 'zustand/middleware'

import type { Workflow } from './types'

interface WorkflowsState {
  workflows: Workflow[]
  // Insert or replace by id, keeping the list newest-first.
  upsert: (w: Workflow) => void
  remove: (id: string) => void
}

const MAX_WORKFLOWS = 200

export const useWorkflowsStore = create<WorkflowsState>()(
  persist(
    (set) => ({
      workflows: [],
      upsert: (w) =>
        set((s) => {
          const rest = s.workflows.filter((x) => x.id !== w.id)
          const list = [{ ...w, updatedAt: Date.now() }, ...rest]
          if (list.length > MAX_WORKFLOWS) list.length = MAX_WORKFLOWS
          return { workflows: list }
        }),
      remove: (id) => set((s) => ({ workflows: s.workflows.filter((x) => x.id !== id) }))
    }),
    { name: 'rev:workflows' }
  )
)
