import { useCallback, useEffect, useRef, useState } from 'react'

import type { AiAction } from '../../../preload'

const KIND_COLOR: Record<AiAction['kind'], string> = {
  navigate: '#a855f7',
  click: '#ff3b30',
  type: '#0a84ff',
  scroll: '#30d158',
  snapshot: '#8e8e93',
  screenshot: '#8e8e93',
  evaluate: '#ff9f0a',
  extract: '#2dd4bf'
}

const KIND_TAG: Record<AiAction['kind'], string> = {
  navigate: 'NAV',
  click: 'CLICK',
  type: 'TYPE',
  scroll: 'SCROLL',
  snapshot: 'SNAP',
  screenshot: 'SHOT',
  evaluate: 'EVAL',
  extract: 'EXTRACT'
}

const MAX_LOG = 5
const TOAST_TTL = 1800
const POS_KEY = 'rev:ai-overlay-pos'
const HIDDEN_KEY = 'rev:ai-overlay-hidden'
const DEFAULT_POS = { x: 12, y: 12 }

interface ToastedAction extends AiAction {
  id: number
}

let nextId = 1

function loadPos(): { x: number; y: number } {
  try {
    const raw = localStorage.getItem(POS_KEY)
    if (!raw) return DEFAULT_POS
    const p = JSON.parse(raw)
    if (typeof p?.x !== 'number' || typeof p?.y !== 'number') return DEFAULT_POS
    return { x: Math.max(0, p.x), y: Math.max(0, p.y) }
  } catch {
    return DEFAULT_POS
  }
}

function loadHidden(): boolean {
  try {
    return localStorage.getItem(HIDDEN_KEY) === '1'
  } catch {
    return false
  }
}

export function AiActionOverlay() {
  const [recent, setRecent] = useState<ToastedAction[]>([])
  const [active, setActive] = useState<ToastedAction | null>(null)
  const [pos, setPos] = useState<{ x: number; y: number }>(loadPos)
  const [hidden, setHidden] = useState<boolean>(loadHidden)
  const [dragging, setDragging] = useState(false)
  const activeTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const dragOffset = useRef<{ dx: number; dy: number } | null>(null)

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

  // Persist position whenever it changes (debounced via rAF on drag end).
  useEffect(() => {
    try {
      localStorage.setItem(POS_KEY, JSON.stringify(pos))
    } catch {}
  }, [pos])

  useEffect(() => {
    try {
      localStorage.setItem(HIDDEN_KEY, hidden ? '1' : '0')
    } catch {}
  }, [hidden])

  const onDragStart = useCallback((e: React.MouseEvent) => {
    e.preventDefault()
    dragOffset.current = { dx: e.clientX - pos.x, dy: e.clientY - pos.y }
    setDragging(true)
  }, [pos.x, pos.y])

  useEffect(() => {
    if (!dragging) return
    const onMove = (e: MouseEvent) => {
      if (!dragOffset.current) return
      const nx = Math.max(0, e.clientX - dragOffset.current.dx)
      const ny = Math.max(0, e.clientY - dragOffset.current.dy)
      // Clamp to viewport so it can't be lost off-screen.
      const maxX = window.innerWidth - 60
      const maxY = window.innerHeight - 30
      setPos({ x: Math.min(nx, maxX), y: Math.min(ny, maxY) })
    }
    const onUp = () => {
      setDragging(false)
      dragOffset.current = null
    }
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup', onUp)
    return () => {
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup', onUp)
    }
  }, [dragging])

  return (
    <>
      {/* Centered active-action toast — separate from the log panel,
          stays ephemeral and non-interactive. */}
      {active && !hidden && (
        <div style={toastStyle}>
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

      {/* Hidden state — minimal "show me" chip pinned to last known position. */}
      {hidden && (
        <button
          onClick={() => setHidden(false)}
          style={{
            position: 'absolute',
            left: pos.x,
            top: pos.y,
            zIndex: 999,
            background: 'var(--glass-panel)',
            color: 'var(--text-dim)',
            border: '1px solid var(--border-2)',
            padding: '3px 9px',
            borderRadius: 999,
            fontSize: 10,
            fontFamily: 'ui-monospace,Menlo,monospace',
            cursor: 'pointer',
            backdropFilter: 'blur(8px)'
          }}
          title="Show AI activity"
        >
          ◷ AI {recent.length > 0 ? recent.length : ''}
        </button>
      )}

      {/* Recent log — floating panel, draggable, hideable. */}
      {!hidden && recent.length > 0 && (
        <div
          style={{
            position: 'absolute',
            left: pos.x,
            top: pos.y,
            zIndex: 999,
            display: 'flex',
            flexDirection: 'column',
            maxWidth: 360,
            background: 'var(--glass-bar)',
            border: '1px solid var(--border)',
            borderRadius: 6,
            backdropFilter: 'blur(8px)',
            boxShadow: '0 4px 16px var(--shadow)',
            userSelect: dragging ? 'none' : 'auto',
            // Allow clicks on this floating panel so the X / drag handle work.
            // (Previous version had pointerEvents: 'none' for the whole stack.)
            pointerEvents: 'auto'
          }}
        >
          {/* Drag handle / header */}
          <div
            onMouseDown={onDragStart}
            style={{
              display: 'flex',
              alignItems: 'center',
              gap: 6,
              padding: '3px 6px 3px 8px',
              borderBottom: '1px solid var(--border)',
              cursor: dragging ? 'grabbing' : 'grab',
              fontSize: 10,
              color: 'var(--text-dim)',
              fontFamily: 'ui-monospace,Menlo,monospace',
              letterSpacing: 0.4
            }}
          >
            <span style={{ flex: 1, textTransform: 'uppercase' }}>
              AI activity · {recent.length}
            </span>
            <button
              onClick={(e) => {
                e.stopPropagation()
                setRecent([])
              }}
              title="Clear"
              style={hdrBtn}
            >
              ⌫
            </button>
            <button
              onClick={(e) => {
                e.stopPropagation()
                setHidden(true)
              }}
              title="Hide"
              style={hdrBtn}
            >
              ✕
            </button>
          </div>

          {/* Rows — no more colored left border. The KIND tag still carries the color. */}
          {recent.map((a) => (
            <div
              key={a.id}
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 6,
                padding: '4px 8px',
                color: 'var(--text-2)',
                fontSize: 11,
                fontFamily: 'ui-monospace,Menlo,monospace',
                opacity: a.id === active?.id ? 1 : 0.65,
                borderBottom: '1px solid var(--glass-hairline)'
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

const toastStyle: React.CSSProperties = {
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
  background: 'var(--glass-bar)',
  color: 'var(--text)',
  fontSize: 12,
  fontWeight: 600,
  letterSpacing: 0.2,
  boxShadow: '0 4px 16px var(--shadow)',
  backdropFilter: 'blur(8px)',
  pointerEvents: 'none',
  animation: 'rev-ai-pop 180ms ease-out'
}

const hdrBtn: React.CSSProperties = {
  background: 'transparent',
  border: 'none',
  color: 'var(--text-dim)',
  cursor: 'pointer',
  fontSize: 11,
  padding: '0 4px',
  lineHeight: 1
}

function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n) + '…' : s
}

function formatTime(ts: number): string {
  const d = new Date(ts)
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}:${String(d.getSeconds()).padStart(2, '0')}`
}
