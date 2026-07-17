// M3 — Visual pipeline. A branch-aware flow of tool nodes and if-conditions,
// rendered as a nested node view and run on the shared executor. Self-contained:
// delete this folder + its line in workflows/index.ts to remove the kind.
import { registerWorkflowKind } from '../core/registry'
import { newWorkflowId, type Workflow } from '../core/types'
import { PipelineEditor } from './PipelineEditor'
import type { PipelineData } from './types'

registerWorkflowKind({
  id: 'pipeline',
  label: 'Pipeline',
  description: 'A branch-aware flow of tool calls with if/then/else conditions.',
  create: (): Workflow => {
    const now = Date.now()
    return {
      id: newWorkflowId(),
      kind: 'pipeline',
      name: '',
      data: { nodes: [], vars: '' } satisfies PipelineData,
      createdAt: now,
      updatedAt: now
    }
  },
  Editor: PipelineEditor
})
