import { useEffect, useRef, useState } from 'react'

import { ACP_AGENTS } from '@/constants'
import { useChatHistory } from '@/stores/chat-history'

interface ChatHistoryMenuProps {
  currentId: string | null
  onOpen: (id: string) => void
  disabled?: boolean
}

function relativeTime(ts: number): string {
  const diff = Date.now() - ts
  const min = Math.floor(diff / 60_000)
  if (min < 1) return 'just now'
  if (min < 60) return `${min}m ago`
  const hr = Math.floor(min / 60)
  if (hr < 24) return `${hr}h ago`
  const day = Math.floor(hr / 24)
  if (day < 7) return `${day}d ago`
  const week = Math.floor(day / 7)
  return `${week}w ago`
}

function agentIcon(id: string): string {
  return ACP_AGENTS.find((a) => a.id === id)?.icon ?? '?'
}

/**
 * Dropdown listing saved conversations, newest first. Clicking a row opens it;
 * the × removes it. Mirrors AgentPicker's popover styling.
 */
export function ChatHistoryMenu({ currentId, onOpen, disabled }: ChatHistoryMenuProps) {
  const [open, setOpen] = useState(false)
  const conversations = useChatHistory((s) => s.conversations)
  const remove = useChatHistory((s) => s.remove)
  const popoverRef = useRef<HTMLDivElement>(null)
  const triggerRef = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (!open) return
    const onDocClick = (e: MouseEvent) => {
      const t = e.target as Node
      if (popoverRef.current?.contains(t) || triggerRef.current?.contains(t)) return
      setOpen(false)
    }
    document.addEventListener('mousedown', onDocClick)
    return () => document.removeEventListener('mousedown', onDocClick)
  }, [open])

  return (
    <div style={{ position: 'relative' }}>
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen((v) => !v)}
        disabled={disabled}
        style={triggerStyle}
        title="Conversation history"
      >
        <span>History</span>
        <span style={{ opacity: 0.6, fontSize: 10 }}>({conversations.length})</span>
        <span style={{ opacity: 0.6, fontSize: 10 }}>▾</span>
      </button>

      {open && (
        <div ref={popoverRef} style={popoverStyle}>
          <header style={popoverHeader}>
            <strong style={{ fontSize: 12 }}>History</strong>
            <span style={{ fontSize: 11, opacity: 0.6 }}>{conversations.length} saved</span>
          </header>
          {conversations.length === 0 ? (
            <p style={{ fontSize: 11, opacity: 0.5, padding: '8px 4px', margin: 0 }}>
              No saved conversations yet.
            </p>
          ) : (
            <ul style={listStyle}>
              {conversations.map((c) => (
                <li key={c.id} style={{ listStyle: 'none' }}>
                  <div
                    style={{
                      ...rowStyle,
                      borderColor: c.id === currentId ? '#4a8ddb' : 'var(--border)'
                    }}
                  >
                    <button
                      type="button"
                      onClick={() => {
                        onOpen(c.id)
                        setOpen(false)
                      }}
                      style={rowMainStyle}
                      title={c.title}
                    >
                      <span style={iconChip}>{agentIcon(c.agentId)}</span>
                      <span style={rowTextStyle}>
                        <span style={rowTitleStyle}>{c.title}</span>
                        <span style={rowMetaStyle}>{relativeTime(c.updatedAt)}</span>
                      </span>
                    </button>
                    <button
                      type="button"
                      onClick={() => remove(c.id)}
                      style={deleteStyle}
                      title="Delete conversation"
                      aria-label="Delete conversation"
                    >
                      ×
                    </button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  )
}

const triggerStyle: React.CSSProperties = {
  display: 'inline-flex',
  alignItems: 'center',
  gap: 4,
  padding: '3px 8px',
  background: 'var(--bg-bar)',
  border: '1px solid var(--border-2)',
  borderRadius: 4,
  color: 'var(--text-2)',
  cursor: 'pointer',
  fontSize: 11
}

const popoverStyle: React.CSSProperties = {
  position: 'absolute',
  top: 'calc(100% + 6px)',
  right: 0,
  zIndex: 50,
  width: 'min(320px, calc(var(--chat-w, 100vw) - 24px))',
  maxHeight: '70vh',
  overflowY: 'auto',
  background: 'var(--bg-bar)',
  border: '1px solid var(--border)',
  borderRadius: 8,
  padding: 10,
  boxShadow: '0 8px 24px rgba(0, 0, 0, 0.5)'
}

const popoverHeader: React.CSSProperties = {
  display: 'flex',
  justifyContent: 'space-between',
  alignItems: 'center',
  padding: '2px 4px 8px'
}

const listStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: 4,
  margin: 0,
  padding: 0
}

const rowStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'stretch',
  background: 'var(--bg-bar)',
  border: '1px solid var(--border)',
  borderRadius: 6,
  overflow: 'hidden'
}

const rowMainStyle: React.CSSProperties = {
  flex: 1,
  minWidth: 0,
  display: 'flex',
  alignItems: 'center',
  gap: 8,
  padding: '6px 8px',
  background: 'transparent',
  border: 'none',
  color: 'var(--text)',
  cursor: 'pointer',
  textAlign: 'left',
  fontFamily: 'inherit'
}

const iconChip: React.CSSProperties = {
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  flexShrink: 0,
  width: 20,
  height: 20,
  borderRadius: 4,
  background: 'var(--surface-2)',
  fontSize: 11,
  fontWeight: 700
}

const rowTextStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  minWidth: 0,
  gap: 2
}

const rowTitleStyle: React.CSSProperties = {
  fontSize: 12,
  whiteSpace: 'nowrap',
  overflow: 'hidden',
  textOverflow: 'ellipsis'
}

const rowMetaStyle: React.CSSProperties = {
  fontSize: 10,
  opacity: 0.5
}

const deleteStyle: React.CSSProperties = {
  flexShrink: 0,
  width: 28,
  background: 'transparent',
  border: 'none',
  borderLeft: '1px solid var(--border)',
  color: '#c77',
  cursor: 'pointer',
  fontSize: 16,
  lineHeight: 1
}
