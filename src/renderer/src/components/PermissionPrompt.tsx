import { useEffect } from 'react'

import {
  useCurrentPermission,
  usePermissionQueue,
  respondToPermission,
  approveCurrentPermission,
  rejectCurrentPermission
} from '@/ai/acp-permission'

const overlay: React.CSSProperties = {
  position: 'fixed',
  inset: 0,
  zIndex: 1000,
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'center',
  background: 'rgba(0,0,0,0.5)'
}

const card: React.CSSProperties = {
  width: 'min(460px, 90vw)',
  background: 'var(--bg-bar)',
  border: '1px solid var(--border-2)',
  borderRadius: 10,
  padding: 18,
  boxShadow: '0 10px 40px rgba(0,0,0,0.6)',
  color: 'var(--text)'
}

function inputPreview(rawInput: unknown): string | null {
  if (rawInput == null) return null
  try {
    const s = JSON.stringify(rawInput, null, 2)
    return s.length > 600 ? s.slice(0, 600) + '\n…' : s
  } catch {
    return null
  }
}

export function PermissionPrompt() {
  const current = useCurrentPermission()
  const queue = usePermissionQueue()

  // Enter approves (best allow option), Escape rejects — global while a prompt
  // is showing. Covers keyboard-driven approval per the UI edge-case rules.
  useEffect(() => {
    if (!current) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Enter') {
        e.preventDefault()
        approveCurrentPermission()
      } else if (e.key === 'Escape') {
        e.preventDefault()
        rejectCurrentPermission()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [current])

  if (!current) return null

  const { request } = current
  const title = request.toolCall?.title || 'The agent requested permission'
  const preview = inputPreview(request.toolCall?.rawInput)
  const firstAllowIdx = request.options.findIndex((o) => o.kind.startsWith('allow'))

  return (
    <div style={overlay}>
      <div style={card} role="dialog" aria-modal="true" aria-label="Agent permission request">
        <div style={{ fontSize: 11, textTransform: 'uppercase', letterSpacing: 0.5, opacity: 0.55, marginBottom: 8 }}>
          Permission required
          {queue.length > 1 && (
            <span style={{ marginLeft: 8, opacity: 0.7 }}>· {queue.length - 1} more queued</span>
          )}
        </div>

        <div style={{ fontSize: 14, fontWeight: 500, marginBottom: preview ? 10 : 14 }}>{title}</div>

        {preview && (
          <pre
            style={{
              background: 'var(--bg)',
              border: '1px solid var(--border)',
              borderRadius: 6,
              padding: 8,
              fontSize: 11,
              maxHeight: 180,
              overflow: 'auto',
              margin: '0 0 14px',
              whiteSpace: 'pre-wrap',
              wordBreak: 'break-word'
            }}
          >
            {preview}
          </pre>
        )}

        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
          {request.options.map((opt, i) => {
            const isAllow = opt.kind.startsWith('allow')
            return (
              <button
                key={opt.optionId}
                type="button"
                autoFocus={i === (firstAllowIdx === -1 ? 0 : firstAllowIdx)}
                onClick={() => respondToPermission(opt.optionId)}
                style={{
                  padding: '6px 14px',
                  fontSize: 12,
                  borderRadius: 6,
                  cursor: 'pointer',
                  border: `1px solid ${isAllow ? '#2f6b3a' : '#6b2f2f'}`,
                  background: isAllow ? '#1c3a23' : '#3a1c1c',
                  color: isAllow ? '#bfe9c8' : '#e9bfbf'
                }}
              >
                {opt.name}
              </button>
            )
          })}
        </div>

        <div style={{ marginTop: 12, fontSize: 11, opacity: 0.45 }}>Enter = allow · Esc = reject</div>
      </div>
    </div>
  )
}
