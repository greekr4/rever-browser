// M2 — Prompt templates. Reusable prompt snippets that seed the agent chat
// input. Fully self-contained: delete this folder + its line in
// workflows/index.ts to remove the kind entirely.
import { useChatDraft } from '@/stores/chat-draft'

import { registerWorkflowKind } from '../core/registry'
import { newWorkflowId, type Workflow } from '../core/types'
import { TemplateEditor, type TemplateData } from './TemplateEditor'

registerWorkflowKind({
  id: 'template',
  label: 'Template',
  description: 'Reusable prompt inserted into the agent chat input.',
  create: (): Workflow => {
    const now = Date.now()
    return {
      id: newWorkflowId(),
      kind: 'template',
      name: '',
      data: { body: '' } satisfies TemplateData,
      createdAt: now,
      updatedAt: now
    }
  },
  Editor: TemplateEditor,
  actionLabel: 'Use',
  action: (w) => {
    const body = (w.data as TemplateData).body
    if (body.trim()) useChatDraft.getState().push(body)
  }
})
