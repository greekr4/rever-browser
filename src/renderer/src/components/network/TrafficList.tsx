import { useMemo, useState } from 'react'
import { useShallow } from 'zustand/react/shallow'

import { useChatDraft } from '@/stores/chat-draft'
import { useRepeaterStore } from '@/stores/repeater'
import { useTrafficStore } from '@/stores/traffic'
import type { TrafficEntry } from '@/types/traffic'

const STATIC_TYPES = new Set(['Image', 'Stylesheet', 'Font', 'Media'])
const QUICK_TYPES = ['XHR', 'Fetch', 'Document', 'Script'] as const

function buildPrefill(rows: TrafficEntry[]): string {
  const lines = rows.map(
    (r) => `- ${r.method} ${r.url} (id: ${r.requestId}${r.status ? `, status: ${r.status}` : ''})`
  )
  return `Analyze these requests. Use get_request for details if needed:\n${lines.join('\n')}\n\nQuestion: `
}

export function TrafficList() {
  const {
    entries,
    order,
    selected,
    detailId,
    clear,
    toggleSelect,
    selectRange,
    clearSelection,
    openDetail,
    closeDetail
  } = useTrafficStore(
    useShallow((s) => ({
      entries: s.entries,
      order: s.order,
      selected: s.selected,
      detailId: s.detailId,
      clear: s.clear,
      toggleSelect: s.toggleSelect,
      selectRange: s.selectRange,
      clearSelection: s.clearSelection,
      openDetail: s.openDetail,
      closeDetail: s.closeDetail
    }))
  )
  const pushDraft = useChatDraft((s) => s.push)
  const sendToRepeater = useRepeaterStore((s) => s.loadFromTraffic)
  const [search, setSearch] = useState('')
  const [typeFilter, setTypeFilter] = useState<Set<string>>(new Set())
  const [hideStatic, setHideStatic] = useState(true)

  const fullList = order.map((id) => entries[id]).filter(Boolean)
  const list = useMemo(() => {
    const q = search.trim().toLowerCase()
    return fullList.filter((e) => {
      if (hideStatic && STATIC_TYPES.has(e.resourceType)) return false
      if (typeFilter.size > 0 && !typeFilter.has(e.resourceType)) return false
      if (q && !e.url.toLowerCase().includes(q) && !e.method.toLowerCase().includes(q)) return false
      return true
    })
  }, [fullList, search, typeFilter, hideStatic])

  const toggleType = (t: string) => {
    setTypeFilter((prev) => {
      const next = new Set(prev)
      if (next.has(t)) next.delete(t)
      else next.add(t)
      return next
    })
  }

  const onCheckbox = (id: string, e: React.MouseEvent) => {
    e.stopPropagation()
    if (e.shiftKey) selectRange(id)
    else toggleSelect(id)
  }

  const onRowClick = (id: string) => {
    if (detailId === id) closeDetail()
    else openDetail(id)
  }

  const onAskAbout = () => {
    const rows = Array.from(selected)
      .map((id) => entries[id])
      .filter(Boolean)
    if (rows.length === 0) return
    pushDraft(buildPrefill(rows))
    clearSelection()
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', position: 'relative' }}>
      <header
        style={{
          padding: '8px 12px',
          borderBottom: '1px solid #2a2a2a',
          display: 'flex',
          flexDirection: 'column',
          gap: 6,
          fontSize: 12
        }}
      >
        <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
          <strong>
            Traffic ({list.length}
            {list.length !== fullList.length ? ` / ${fullList.length}` : ''})
          </strong>
          <button
            onClick={() => {
              clear()
              void window.rev.traffic.clear()
            }}
            style={{ marginLeft: 'auto', fontSize: 11 }}
            title="Clear captured traffic"
          >
            Clear
          </button>
        </div>
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Filter URL or method…"
          style={{ padding: '4px 8px', fontSize: 11 }}
        />
        <div style={{ display: 'flex', gap: 4, flexWrap: 'wrap', alignItems: 'center' }}>
          {QUICK_TYPES.map((t) => {
            const active = typeFilter.has(t)
            return (
              <button
                key={t}
                onClick={() => toggleType(t)}
                style={{
                  padding: '2px 8px',
                  fontSize: 10,
                  borderRadius: 10,
                  background: active ? '#2a4a6a' : '#1a1a1a',
                  borderColor: active ? '#4a7ab0' : '#333'
                }}
              >
                {t}
              </button>
            )
          })}
          <label
            style={{
              marginLeft: 'auto',
              display: 'flex',
              gap: 4,
              alignItems: 'center',
              fontSize: 10,
              opacity: 0.8,
              cursor: 'pointer'
            }}
          >
            <input
              type="checkbox"
              checked={hideStatic}
              onChange={(e) => setHideStatic(e.target.checked)}
            />
            hide static
          </label>
        </div>
      </header>
      <div
        style={{
          flex: 1,
          overflowY: 'auto',
          fontFamily: 'ui-monospace, monospace',
          fontSize: 11
        }}
      >
        {list.length === 0 && (
          <p style={{ padding: 12, opacity: 0.5 }}>Open a page — traffic will appear here.</p>
        )}
        <table style={{ width: '100%', borderCollapse: 'collapse' }}>
          <thead style={{ position: 'sticky', top: 0, background: '#1a1a1a', zIndex: 1 }}>
            <tr>
              <th style={{ ...th, width: 24, padding: '6px 4px' }}></th>
              <th style={th}>Method</th>
              <th style={th}>Status</th>
              <th style={th}>Type</th>
              <th style={{ ...th, width: '100%' }}>URL</th>
              <th style={th}>Size</th>
              <th style={{ ...th, width: 28 }}></th>
            </tr>
          </thead>
          <tbody>
            {list.map((e) => {
              const isSelected = selected.has(e.requestId)
              const isActive = detailId === e.requestId
              return (
                <tr
                  key={e.requestId}
                  onClick={() => onRowClick(e.requestId)}
                  style={{
                    borderBottom: '1px solid #1f1f1f',
                    background: isActive ? '#1d2a3a' : isSelected ? '#1a1f1a' : undefined,
                    cursor: 'pointer'
                  }}
                >
                  <td style={{ ...td, padding: '4px 4px' }} onClick={(ev) => ev.stopPropagation()}>
                    <input
                      type="checkbox"
                      checked={isSelected}
                      onChange={() => {
                        /* handled by onClick */
                      }}
                      onClick={(ev) => onCheckbox(e.requestId, ev)}
                      style={{ cursor: 'pointer' }}
                    />
                  </td>
                  <td style={td}>{e.method}</td>
                  <td style={{ ...td, color: statusColor(e.status) }}>{e.status ?? '·'}</td>
                  <td style={{ ...td, opacity: 0.7 }}>{e.resourceType}</td>
                  <td
                    style={{
                      ...td,
                      maxWidth: 0,
                      whiteSpace: 'nowrap',
                      overflow: 'hidden',
                      textOverflow: 'ellipsis'
                    }}
                    title={e.url}
                  >
                    {e.url}
                  </td>
                  <td style={{ ...td, textAlign: 'right' }}>
                    {e.encodedDataLength !== undefined ? formatBytes(e.encodedDataLength) : '·'}
                  </td>
                  <td style={{ ...td, padding: '2px 4px' }} onClick={(ev) => ev.stopPropagation()}>
                    <button
                      onClick={() => void sendToRepeater(e.requestId)}
                      title="Send to Repeater"
                      style={{ fontSize: 10, padding: '1px 6px', lineHeight: 1.2 }}
                    >
                      ↻R
                    </button>
                  </td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>

      {selected.size > 0 && (
        <div
          style={{
            position: 'absolute',
            left: 12,
            right: 12,
            bottom: 12,
            padding: '8px 12px',
            background: '#1f2a1f',
            border: '1px solid #2f4a2f',
            borderRadius: 6,
            display: 'flex',
            alignItems: 'center',
            gap: 10,
            fontSize: 12,
            boxShadow: '0 2px 12px rgba(0,0,0,0.5)'
          }}
        >
          <strong>{selected.size} selected</strong>
          <button onClick={onAskAbout} style={{ marginLeft: 'auto' }}>
            Ask about
          </button>
          <button onClick={clearSelection}>Clear</button>
        </div>
      )}
    </div>
  )
}

const th: React.CSSProperties = {
  textAlign: 'left',
  padding: '6px 8px',
  borderBottom: '1px solid #2a2a2a',
  fontWeight: 'normal',
  opacity: 0.7
}

const td: React.CSSProperties = {
  padding: '4px 8px'
}

function statusColor(status?: number): string {
  if (!status) return '#888'
  if (status >= 500) return '#f55'
  if (status >= 400) return '#ec9'
  if (status >= 300) return '#a9f'
  return '#9d9'
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n}B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)}KB`
  return `${(n / 1024 / 1024).toFixed(1)}MB`
}
