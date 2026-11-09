import { useEffect, useRef, useState } from 'react'

import { ACP_AGENTS } from '@/constants'
import { ProviderIcon } from './ProviderIcon'
import { useChatHistory } from '@/stores/chat-history'

import { useI18nStore, useT } from '@/stores/i18n'
interface ChatHistoryMenuProps {
  currentId: string | null
  onOpen: (id: string) => void
  disabled?: boolean
}

function relativeTime(ts: number, lang: string): string {
  const rtf = new Intl.RelativeTimeFormat(lang, { numeric: 'auto', style: 'short' })
  const min = Math.round((ts - Date.now()) / 60_000)
  if (min === 0) return rtf.format(0, 'second') // "now" — 0 minutes reads oddly in Korean
  if (min > -60) return rtf.format(min, 'minute')
  const hr = Math.round(min / 60)
  if (hr > -24) return rtf.format(hr, 'hour')
  const day = Math.round(hr / 24)
  return day > -7 ? rtf.format(day, 'day') : rtf.format(Math.round(day / 7), 'week')
}


/**
 * Dropdown listing saved conversations, newest first. Clicking a row opens it;
 * the × removes it. Mirrors AgentPicker's popover styling.
 */
export function ChatHistoryMenu({ currentId, onOpen, disabled }: ChatHistoryMenuProps) {
  const tr = useT()
  const [open, setOpen] = useState(false)
  const lang = useI18nStore((s) => s.lang)
  const conversations = useChatHistory((s) => s.conversations)
  const remove = useChatHistory((s) => s.remove)
  const popoverRef = useRef<HTMLDivElement>(null)
  const triggerRef = useRef<HTMLButtonElement>(null)
  // Fixed and clamped to the chat column: an absolute popover hanging left of the button got clipped.
  const [pos, setPos] = useState<React.CSSProperties>({})
  const toggle = (): void => {
    if (!open && triggerRef.current) {
      const r = triggerRef.current.getBoundingClientRect()
      const col = triggerRef.current.closest('aside')?.getBoundingClientRect() ?? { left: 0, right: window.innerWidth }
      const width = Math.min(320, col.right - col.left - 16)
      const left = Math.max(col.left + 8, Math.min(r.right - width, col.right - width - 8))
      setPos({ top: r.bottom + 6, left, width })
    }
    setOpen((v) => !v)
  }

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
        onClick={toggle}
        disabled={disabled}
        className="btn-ghost"
        style={{ display: 'inline-flex', alignItems: 'center', gap: 4 }}
        title={tr('chat.historyTitle')}
      >
        <span>{tr('chat.history')}</span>
        <span style={{ opacity: 0.6, fontSize: 10 }}>({conversations.length})</span>
        <span style={{ opacity: 0.6, fontSize: 10 }}>▾</span>
      </button>

      {open && (
        <div ref={popoverRef} style={{ ...popoverStyle, ...pos }}>
          <header style={popoverHeader}>
            <strong style={{ fontSize: 12 }}>{tr('chat.history')}</strong>
            <span style={{ fontSize: 11, opacity: 0.6 }}>{tr('chat.historySaved', { n: conversations.length })}</span>
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
                      borderColor: c.id === currentId ? 'var(--accent)' : 'var(--border)'
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
                      <span style={iconChip} title={ACP_AGENTS.find((a) => a.id === c.agentId)?.name}>
                        <ProviderIcon agentId={c.agentId} size={14} fallback={ACP_AGENTS.find((a) => a.id === c.agentId)?.icon ?? '?'} />
                      </span>
                      <span style={rowTextStyle}>
                        <span style={rowTitleStyle}>{c.title}</span>
                        <span style={rowMetaStyle}>{relativeTime(c.updatedAt, lang)}</span>
                      </span>
                    </button>
                    <button
                      type="button"
                      onClick={() => remove(c.id)}
                      className="btn-danger btn-icon"
                      style={{ flexShrink: 0, alignSelf: 'center', marginRight: 4, fontSize: 15 }}
                      title={tr('chat.deleteConversation')}
                      aria-label={tr('chat.deleteConversation')}
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

const popoverStyle: React.CSSProperties = {
  position: 'fixed',
  boxSizing: 'border-box',
  zIndex: 50,
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

