// M1 — Macro (record & replay, v1: hand-authored + deterministic replay).
// A named sequence of MCP tool calls that runs with no LLM in the loop. The
// Editor doubles as the runner (Run button + per-step results). Self-contained:
// delete this folder + its line in workflows/index.ts to remove the kind.
import { registerWorkflowKind } from '../core/registry'
import { newWorkflowId, type Workflow } from '../core/types'
import { MacroEditor, type MacroData } from './MacroEditor'

registerWorkflowKind({
  id: 'macro',
  label: 'Macro',
  description: 'A saved sequence of tool calls, replayed deterministically.',
  create: (): Workflow => {
    const now = Date.now()
    return {
      id: newWorkflowId(),
      kind: 'macro',
      name: '',
      data: { steps: [], vars: '' } satisfies MacroData,
      createdAt: now,
      updatedAt: now
    }
  },
  Editor: MacroEditor
  // No list action — running happens inside the Editor so per-step results are
  // visible. Open a macro via "Edit" and click "Run macro".
})
