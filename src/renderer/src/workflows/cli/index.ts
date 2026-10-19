import { registerWorkflowKind } from '../core/registry'
import { newWorkflowId, type Workflow } from '../core/types'
import type { CliData } from '../../../../shared/cli-spec'
import { CliEditor } from './CliEditor'

const empty = (): CliData => ({
  site: '',
  title: '',
  baseUrl: '',
  commands: [],
  writes: false
})

registerWorkflowKind({
  id: 'cli',
  label: 'CLI',
  description: 'Commands compiled from the API calls you actually made.',
  create: (): Workflow => {
    const now = Date.now()
    return {
      id: newWorkflowId(),
      kind: 'cli',
      name: '',
      data: empty(),
      createdAt: now,
      updatedAt: now
    }
  },
  Editor: CliEditor
})
