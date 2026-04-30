import { useEffect, useRef, useState } from 'react'

import type { AiAction } from '../../../preload'

const KIND_COLOR: Record<AiAction['kind'], string> = {
  navigate: '#a855f7',
  click: '#ff3b30',
  type: '#0a84ff',
  scroll: '#30d158',
  snapshot: '#8e8e93',
  screenshot: '#8e8e93',
  evaluate: '#ff9f0a'
}

const KIND_TAG: Record<AiAction['kind'], string> = {
  navigate: 'NAV',
  click: 'CLICK',
  type: 'TYPE',
  scroll: 'SCROLL',
  snapshot: 'SNAP',
  screenshot: 'SHOT',
  evaluate: 'EVAL'
}

const MAX_LOG = 5
const TOAST_TTL = 1800

interface ToastedAction extends AiAction {
  id: number
}

let nextId = 1

export function AiActionOverlay() {
  const [recent, setRecent] = useState<ToastedAction[]>([])
  const [active, setActive] = useState<ToastedAction | null>(null)
  const activeTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    const off = window.rev.aiAction.subscribe((action) => {
      const t: ToastedAction = { ...action, id: nextId++ }
      setActive(t)
      setRecent((prev) => [t, ...prev].slice(0, MAX_LOG))
      if (activeTimer.current) clearTimeout(activeTimer.current)
      activeTimer.current = setTimeout(() => setActive(null), TOAST_TTL)
    })
    return () => {
      off()
      if (activeTimer.current) clearTimeout(activeTimer.current)
    }
  }, [])

  return (
    <>
      {active && (
        <div
          style={{
            position: 'absolute',
            top: 8,
            left: '50%',
            transform: 'translateX(-50%)',
            zIndex: 1000,
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            padding: '6px 12px',
            borderRadius: 999,
            background: 'rgba(20, 20, 22, 0.92)',
            color: '#fff',
            fontSize: 12,
            fontWeight: 600,
            letterSpacing: 0.2,
            boxShadow: '0 4px 16px rgba(0,0,0,0.35)',
            backdropFilter: 'blur(8px)',
            pointerEvents: 'none',
            animation: 'rev-ai-pop 180ms ease-out'
          }}
        >
          <span
            style={{
              width: 8,
              height: 8,
              borderRadius: '50%',
              background: KIND_COLOR[active.kind],
              boxShadow: `0 0 8px ${KIND_COLOR[active.kind]}`,
              animation: 'rev-ai-blink 1s ease-in-out infinite'
            }}
          />
          <span
            style={{
              fontSize: 10,
              fontWeight: 700,
              letterSpacing: 0.6,
              padding: '2px 6px',
              borderRadius: 3,
              background: KIND_COLOR[active.kind],
              color: '#fff'
            }}
          >
            {KIND_TAG[active.kind]}
          </span>
          <span>{active.label}</span>
          {active.detail && (
            <span style={{ opacity: 0.6, fontWeight: 400, maxWidth: 320 }}>
              {truncate(active.detail, 60)}
            </span>
          )}
        </div>
      )}

      {recent.length > 0 && (
        <div
          style={{
            position: 'absolute',
            top: 12,
            left: 12,
            zIndex: 999,
            display: 'flex',
            flexDirection: 'column',
            gap: 4,
            maxWidth: 320,
            pointerEvents: 'none'
          }}
        >
          {recent.map((a) => (
            <div
              key={a.id}
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 6,
                padding: '4px 8px',
                borderRadius: 4,
                background: 'rgba(20,20,22,0.78)',
                color: '#ddd',
                fontSize: 11,
                fontFamily: 'ui-monospace,Menlo,monospace',
                borderLeft: `2px solid ${KIND_COLOR[a.kind]}`,
                opacity: a.id === active?.id ? 1 : 0.65
              }}
            >
              <span
                style={{
                  color: KIND_COLOR[a.kind],
                  fontWeight: 700,
                  minWidth: 44,
                  letterSpacing: 0.4
                }}
              >
                {KIND_TAG[a.kind]}
              </span>
              <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {a.label}
                {a.detail ? ` · ${truncate(a.detail, 40)}` : ''}
              </span>
              <span style={{ opacity: 0.5 }}>{formatTime(a.ts)}</span>
            </div>
          ))}
        </div>
      )}

      <style>{`
        @keyframes rev-ai-pop  { from { transform: translate(-50%, -8px); opacity: 0 } to { transform: translate(-50%, 0); opacity: 1 } }
        @keyframes rev-ai-blink { 0%,100% { opacity: 1 } 50% { opacity: 0.3 } }
      `}</style>
    </>
  )
}

function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n) + '…' : s
}

function formatTime(ts: number): string {
  const d = new Date(ts)
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}:${String(d.getSeconds()).padStart(2, '0')}`
}
