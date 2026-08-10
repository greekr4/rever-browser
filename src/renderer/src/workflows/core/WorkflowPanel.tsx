import { useEffect, useState } from 'react'

import { getWorkflowKind, listWorkflowKinds, type WorkflowKind } from './registry'
import { useWorkflowsStore } from './store'
import type { Workflow } from './types'

import { useT } from '@/stores/i18n'
// Host panel for all workflow kinds. It knows nothing about individual kinds —
// it lists whatever modules have registered and delegates editing to each
// kind's Editor.
export function WorkflowPanel(): React.ReactElement {
  const tr = useT()
  const workflows = useWorkflowsStore((s) => s.workflows)
  const upsert = useWorkflowsStore((s) => s.upsert)
  const remove = useWorkflowsStore((s) => s.remove)

  // The workflow currently being created/edited (a working copy).
  const [draft, setDraft] = useState<Workflow | null>(null)
  // Per-row state for the list's action button (no editor open to report into).
  const [runningId, setRunningId] = useState<string | null>(null)
  const [rowError, setRowError] = useState<Record<string, string>>({})
  // Workflow awaiting delete confirmation. Deleting is irreversible (the store
  // persists to localStorage), so ✕ asks first.
  const [confirmId, setConfirmId] = useState<string | null>(null)
  const pendingDelete = confirmId ? workflows.find((w) => w.id === confirmId) ?? null : null

  useEffect(() => {
    if (!pendingDelete) return
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') setConfirmId(null)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [pendingDelete])

  const runFromList = async (kind: WorkflowKind, w: Workflow): Promise<void> => {
    if (!kind.action) return
    setRunningId(w.id)
    setRowError((prev) => {
      const next = { ...prev }
      delete next[w.id]
      return next
    })
    try {
      await kind.action(w)
    } catch (e) {
      setRowError((prev) => ({ ...prev, [w.id]: e instanceof Error ? e.message : String(e) }))
    } finally {
      setRunningId((cur) => (cur === w.id ? null : cur))
    }
  }

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
      <div
        style={{
          padding: 10,
          fontSize: 12,
          display: 'flex',
          flexDirection: 'column',
          gap: 10,
          height: '100%',
          // Without this the padding is added on top of 100% and the panel's
          // overflow:hidden clips the last 24px (hiding the Run button).
          boxSizing: 'border-box'
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontWeight: 600 }}>{kind?.label ?? draft.kind}</span>
          <div style={{ marginLeft: 'auto', display: 'flex', gap: 6 }}>
            <button type="button" onClick={() => setDraft(null)}>{tr('common.cancel')}</button>
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
    <div style={{ padding: 10, fontSize: 12, display: 'flex', flexDirection: 'column', gap: 10, height: '100%', boxSizing: 'border-box' }}>
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
        <div className="panel-empty">
          No saved workflows yet.
        </div>
      ) : (
        <div style={{ flex: 1, overflowY: 'auto', display: 'flex', flexDirection: 'column', gap: 4 }}>
          {workflows.map((w) => {
            const kind = getWorkflowKind(w.kind)
            const busy = runningId === w.id
            const error = rowError[w.id]
            return (
              <div
                key={w.id}
                style={{
                  display: 'flex',
                  flexWrap: 'wrap',
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
                    onClick={() => void runFromList(kind, w)}
                    disabled={busy}
                    style={{ background: 'var(--accent-soft)', borderColor: 'var(--accent-border)', color: 'var(--accent-text)' }}
                  >
                    {busy ? 'Running…' : kind.actionLabel ?? 'Run'}
                  </button>
                )}
                <button type="button" onClick={() => setDraft(w)} title={tr('common.edit')}>{tr('common.edit')}</button>
                <button type="button" onClick={() => setConfirmId(w.id)} title={tr('common.delete')}>✕</button>
                {error && (
                  <div style={{ flexBasis: '100%', color: '#e06c6c', fontSize: 11 }}>{error}</div>
                )}
              </div>
            )
          })}
        </div>
      )}

      {/* Compact delete confirmation. Fixed rather than absolute so the panel's
          overflow can't clip it. Escape or a backdrop click cancels; the Delete
          button takes focus, so Enter confirms. */}
      {pendingDelete && (
        <div
          onClick={() => setConfirmId(null)}
          style={{
            position: 'fixed',
            inset: 0,
            zIndex: 200,
            background: 'rgba(0,0,0,0.45)',
            display: 'grid',
            placeItems: 'center'
          }}
        >
          <div
            onClick={(e) => e.stopPropagation()}
            role="dialog"
            aria-modal="true"
            style={{
              width: 280,
              padding: 14,
              display: 'flex',
              flexDirection: 'column',
              gap: 10,
              background: 'var(--surface)',
              border: '1px solid var(--border-2)',
              borderRadius: 8,
              boxShadow: '0 12px 40px var(--shadow)'
            }}
          >
            <div style={{ fontSize: 13, fontWeight: 600 }}>Delete this workflow?</div>
            <div
              style={{
                fontSize: 12,
                color: 'var(--text-dim)',
                overflow: 'hidden',
                textOverflow: 'ellipsis',
                whiteSpace: 'nowrap'
              }}
              title={pendingDelete.name}
            >
              {pendingDelete.name || 'Untitled'}
            </div>
            <div style={{ display: 'flex', gap: 6, justifyContent: 'flex-end' }}>
              <button type="button" onClick={() => setConfirmId(null)}>
                Cancel
              </button>
              <button
                type="button"
                autoFocus
                onClick={() => {
                  remove(pendingDelete.id)
                  setConfirmId(null)
                }}
                style={{
                  background: 'var(--chip-danger-bg)',
                  borderColor: 'var(--chip-danger-border)',
                  color: 'var(--chip-danger-text)'
                }}
              >
                Delete
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
