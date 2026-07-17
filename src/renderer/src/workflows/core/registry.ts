import type { ComponentType } from 'react'

import type { Workflow } from './types'

export interface WorkflowEditorProps {
  workflow: Workflow
  onChange: (w: Workflow) => void
}

// A pluggable workflow kind. Modules self-register one of these at import time,
// so adding/removing a module is a folder + one barrel import line — the core
// discovers kinds purely through this registry.
export interface WorkflowKind {
  id: string
  label: string
  description: string
  // Build a blank workflow of this kind.
  create: () => Workflow
  // Editor rendered when a workflow of this kind is opened.
  Editor: ComponentType<WorkflowEditorProps>
  // Optional primary action shown per-item in the list (e.g. "Use", "Run").
  actionLabel?: string
  action?: (workflow: Workflow) => void | Promise<void>
}

const kinds = new Map<string, WorkflowKind>()

export function registerWorkflowKind(kind: WorkflowKind): void {
  kinds.set(kind.id, kind)
}

export function getWorkflowKind(id: string): WorkflowKind | undefined {
  return kinds.get(id)
}

export function listWorkflowKinds(): WorkflowKind[] {
  return [...kinds.values()]
}
