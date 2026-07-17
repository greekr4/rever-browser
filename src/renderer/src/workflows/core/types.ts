// A saved workflow. The core stays deliberately ignorant of each kind's shape:
// `kind` is a plain string and `data` is opaque, so a workflow module can be
// added or deleted without the core (store/panel) ever referencing its types.
export interface Workflow {
  id: string
  kind: string
  name: string
  description?: string
  // Kind-specific payload. Each module casts this to its own type.
  data: unknown
  createdAt: number
  updatedAt: number
}

export function newWorkflowId(): string {
  return `w${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`
}
