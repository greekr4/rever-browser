import { useMemo, useState } from 'react'

import { useHistoryStore, type HistoryEntry } from '@/stores/history'
import { useNavigationRequestStore } from '@/stores/navigation-request'

export function HistoryPanel() {
  const entries = useHistoryStore((s) => s.entries)
  const clear = useHistoryStore((s) => s.clear)
  const requestNav = useNavigationRequestStore((s) => s.request)

  const [filter, setFilter] = useState('')

  const filtered = useMemo(() => {
    const q = filter.trim().toLowerCase()
    const sorted = [...entries].sort((a, b) => b.visitedAt - a.visitedAt)
    if (!q) return sorted
    return sorted.filter(
      (e) => e.url.toLowerCase().includes(q) || e.title.toLowerCase().includes(q)
    )
  }, [entries, filter])

  const grouped = useMemo(() => {
    const groups: Array<{ label: string; items: HistoryEntry[] }> = []
    let currentLabel = ''
    let currentItems: HistoryEntry[] = []
    for (const e of filtered) {
      const label = dayLabel(e.visitedAt)
      if (label !== currentLabel) {
        if (currentItems.length) groups.push({ label: currentLabel, items: currentItems })
        currentLabel = label
        currentItems = []
      }
      currentItems.push(e)
    }
    if (currentItems.length) groups.push({ label: currentLabel, items: currentItems })
    return groups
  }, [filtered])

  const openEntry = (entry: HistoryEntry) => {
    requestNav(entry.url)
  }

  return (
    <div
      style={{
        padding: 10,
        fontSize: 12,
        color: '#ddd',
        height: '100%',
        display: 'flex',
        flexDirection: 'column',
        minHeight: 0
      }}
    >
      <div style={{ display: 'flex', gap: 8, marginBottom: 10, alignItems: 'center' }}>
        <input
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Filter by URL or title"
          style={{
            flex: 1,
            background: '#111',
            color: '#eee',
            border: '1px solid #333',
            padding: '4px 8px',
            fontSize: 11,
            borderRadius: 3,
            fontFamily: 'ui-monospace, monospace'
          }}
        />
        <span style={{ opacity: 0.55, fontSize: 11 }}>{filtered.length} entries</span>
        <button
          onClick={() => {
            if (entries.length === 0) return
            if (!confirm('Clear all history?')) return
            clear()
          }}
          style={btnStyle}
          disabled={entries.length === 0}
        >
          Clear
        </button>
      </div>

      <div
        style={{
          flex: 1,
          overflow: 'auto',
          border: '1px solid #2a2a2a',
          borderRadius: 4,
          background: '#161616'
        }}
      >
        {filtered.length === 0 && (
          <div style={{ padding: 24, textAlign: 'center', opacity: 0.5, fontSize: 11 }}>
            {entries.length === 0 ? 'No history yet — navigate somewhere to start.' : 'No matches.'}
          </div>
        )}
        {grouped.map((group) => (
          <div key={group.label}>
            <div
              style={{
                position: 'sticky',
                top: 0,
                background: '#1a1a1a',
                padding: '4px 10px',
                fontSize: 10,
                fontWeight: 600,
                color: '#8aa',
                borderBottom: '1px solid #2a2a2a',
                textTransform: 'uppercase',
                letterSpacing: 0.5
              }}
            >
              {group.label}
            </div>
            {group.items.map((entry) => (
              <button
                key={entry.id}
                onClick={() => openEntry(entry)}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 10,
                  width: '100%',
                  padding: '6px 10px',
                  background: 'transparent',
                  border: 'none',
                  borderBottom: '1px solid #1f1f1f',
                  color: '#ddd',
                  textAlign: 'left',
                  cursor: 'pointer',
                  fontSize: 11
                }}
                onMouseEnter={(e) => (e.currentTarget.style.background = '#1f1f1f')}
                onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}
              >
                <span style={{ opacity: 0.55, width: 50, fontFamily: 'ui-monospace, monospace' }}>
                  {timeLabel(entry.visitedAt)}
                </span>
                <Favicon url={entry.url} />
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div
                    style={{
                      whiteSpace: 'nowrap',
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      color: '#eee'
                    }}
                  >
                    {entry.title || hostnameOf(entry.url)}
                  </div>
                  <div
                    style={{
                      whiteSpace: 'nowrap',
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      opacity: 0.55,
                      fontFamily: 'ui-monospace, monospace',
                      fontSize: 10,
                      marginTop: 1
                    }}
                  >
                    {entry.url}
                  </div>
                </div>
              </button>
            ))}
          </div>
        ))}
      </div>
    </div>
  )
}

function Favicon({ url }: { url: string }) {
  let host = ''
  try {
    host = new URL(url).hostname
  } catch {
    /* ignore */
  }
  if (!host) {
    return (
      <span
        style={{
          width: 16,
          height: 16,
          background: '#333',
          borderRadius: 2,
          flexShrink: 0
        }}
      />
    )
  }
  return (
    <img
      src={`https://www.google.com/s2/favicons?domain=${host}&sz=32`}
      width={16}
      height={16}
      alt=""
      style={{ flexShrink: 0, borderRadius: 2 }}
      onError={(e) => {
        e.currentTarget.style.visibility = 'hidden'
      }}
    />
  )
}

function dayLabel(ts: number): string {
  const d = new Date(ts)
  const today = new Date()
  const yesterday = new Date()
  yesterday.setDate(today.getDate() - 1)
  const sameDay = (a: Date, b: Date) =>
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  if (sameDay(d, today)) return 'Today'
  if (sameDay(d, yesterday)) return 'Yesterday'
  return d.toISOString().slice(0, 10)
}

function timeLabel(ts: number): string {
  const d = new Date(ts)
  return d.toTimeString().slice(0, 5)
}

function hostnameOf(url: string): string {
  try {
    return new URL(url).hostname
  } catch {
    return url
  }
}

const btnStyle: React.CSSProperties = {
  background: '#1a1a1a',
  border: '1px solid #333',
  color: '#ccc',
  padding: '4px 10px',
  borderRadius: 3,
  fontSize: 11,
  cursor: 'pointer'
}
