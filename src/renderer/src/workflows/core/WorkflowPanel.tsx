import { useState } from 'react'

import { getWorkflowKind, listWorkflowKinds, type WorkflowKind } from './registry'
import { useWorkflowsStore } from './store'
import type { Workflow } from './types'

// Host panel for all workflow kinds. It knows nothing about individual kinds —
// it lists whatever modules have registered and delegates editing to each
// kind's Editor.
export function WorkflowPanel(): React.ReactElement {
  const workflows = useWorkflowsStore((s) => s.workflows)
  const upsert = useWorkflowsStore((s) => s.upsert)
  const remove = useWorkflowsStore((s) => s.remove)

  // The workflow currently being created/edited (a working copy).
  const [draft, setDraft] = useState<Workflow | null>(null)

  const kinds = listWorkflowKinds()

  const startNew = (kind: WorkflowKind): void => setDraft(kind.create())
  const save = (): void => {
    if (draft) upsert(draft)
    setDraft(null)
  }

  if (draft) {
    const kind = getWorkflowKind(draft.kind)
    const Editor = kind?.Editor
    return (
      <div style={{ padding: 12, display: 'flex', flexDirection: 'column', gap: 10, height: '100%' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontWeight: 600 }}>{kind?.label ?? draft.kind}</span>
          <div style={{ marginLeft: 'auto', display: 'flex', gap: 6 }}>
            <button type="button" onClick={() => setDraft(null)}>Cancel</button>
            <button
              type="button"
              onClick={save}
              disabled={!draft.name.trim()}
              style={{ background: 'var(--accent)', color: '#fff', borderColor: 'var(--accent)' }}
            >
              Save
            </button>
          </div>
        </div>
        {Editor ? (
          <div style={{ flex: 1, overflowY: 'auto' }}>
            <Editor workflow={draft} onChange={setDraft} />
          </div>
        ) : (
          <div style={{ color: 'var(--text-dim)' }}>
            No editor registered for kind “{draft.kind}”.
          </div>
        )}
      </div>
    )
  }

  return (
    <div style={{ padding: 12, display: 'flex', flexDirection: 'column', gap: 10, height: '100%' }}>
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6, alignItems: 'center' }}>
        <span style={{ color: 'var(--text-dim)', marginRight: 4 }}>New:</span>
        {kinds.length === 0 && (
          <span style={{ color: 'var(--text-dim)' }}>No workflow kinds installed.</span>
        )}
        {kinds.map((k) => (
          <button key={k.id} type="button" onClick={() => startNew(k)} title={k.description}>
            + {k.label}
          </button>
        ))}
      </div>

      {workflows.length === 0 ? (
        <div style={{ color: 'var(--text-dim)', fontStyle: 'italic', padding: '8px 2px' }}>
          No saved workflows yet.
        </div>
      ) : (
        <div style={{ flex: 1, overflowY: 'auto', display: 'flex', flexDirection: 'column', gap: 4 }}>
          {workflows.map((w) => {
            const kind = getWorkflowKind(w.kind)
            return (
              <div
                key={w.id}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 8,
                  padding: '6px 8px',
                  border: '1px solid var(--border)',
                  borderRadius: 6,
                  background: 'var(--surface)'
                }}
              >
                <span
                  style={{
                    fontSize: 10,
                    textTransform: 'uppercase',
                    letterSpacing: 0.4,
                    color: 'var(--text-dim)',
                    minWidth: 58
                  }}
                >
                  {kind?.label ?? w.kind}
                </span>
                <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {w.name}
                </span>
                {kind?.action && (
                  <button
                    type="button"
                    onClick={() => void kind.action?.(w)}
                    style={{ background: 'var(--accent-soft)', borderColor: 'var(--accent-border)', color: 'var(--accent)' }}
                  >
                    {kind.actionLabel ?? 'Run'}
                  </button>
                )}
                <button type="button" onClick={() => setDraft(w)} title="Edit">Edit</button>
                <button type="button" onClick={() => remove(w.id)} title="Delete">✕</button>
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}
