import { memo, useCallback, useMemo, useState } from 'react'

import { useT } from '@/stores/i18n'
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
  const tr = useT()
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

  // Memoized on the actual inputs — order/entries — so the filter below doesn't
  // rebuild fullList (a fresh array) on every unrelated render/store tick.
  const fullList = useMemo(
    () => order.map((id) => entries[id]).filter(Boolean),
    [order, entries]
  )
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

  const onCheckbox = useCallback(
    (id: string, e: React.MouseEvent) => {
      e.stopPropagation()
      if (e.shiftKey) selectRange(id)
      else toggleSelect(id)
    },
    [selectRange, toggleSelect]
  )

  const onRowClick = useCallback(
    (id: string) => {
      if (detailId === id) closeDetail()
      else openDetail(id)
    },
    [detailId, closeDetail, openDetail]
  )

  const sendTitle = tr('traffic.sendToRepeater')

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
          padding: '6px 10px',
          borderBottom: '1px solid var(--border)',
          display: 'flex',
          flexDirection: 'column',
          gap: 6,
          fontSize: 11,
          flexShrink: 0
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
            title={tr('traffic.clear')}
          >
            Clear
          </button>
        </div>
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder={tr('traffic.filter')}
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
                  background: active ? 'var(--accent-soft)' : 'var(--surface)',
                  borderColor: active ? 'var(--accent-border)' : 'var(--border-2)'
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
          <div className="panel-empty">Open a page — traffic will appear here.</div>
        )}
        <table style={{ width: '100%', borderCollapse: 'collapse' }}>
          <thead style={{ position: 'sticky', top: 0, background: 'var(--surface)', zIndex: 1 }}>
            <tr>
              <th style={{ ...th, width: 24, padding: '6px 4px' }}></th>
              <th style={th}>{tr('traffic.method')}</th>
              <th style={th}>{tr('traffic.status')}</th>
              <th style={th}>{tr('traffic.type')}</th>
              <th style={{ ...th, width: '100%' }}>{tr('traffic.url')}</th>
              <th style={th}>{tr('traffic.size')}</th>
              <th style={{ ...th, width: 28 }}></th>
            </tr>
          </thead>
          <tbody>
            {list.map((e) => (
              <TrafficRow
                key={e.requestId}
                entry={e}
                isSelected={selected.has(e.requestId)}
                isActive={detailId === e.requestId}
                onRowClick={onRowClick}
                onCheckbox={onCheckbox}
                sendToRepeater={sendToRepeater}
                sendTitle={sendTitle}
              />
            ))}
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
            background: 'var(--chip-ok-bg)',
            border: '1px solid var(--chip-ok-border)',
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
          <button onClick={clearSelection}>{tr('common.clear')}</button>
        </div>
      )}
    </div>
  )
}

interface TrafficRowProps {
  entry: TrafficEntry
  isSelected: boolean
  isActive: boolean
  onRowClick: (id: string) => void
  onCheckbox: (id: string, e: React.MouseEvent) => void
  sendToRepeater: (id: string) => void | Promise<void>
  sendTitle: string
}

// Memoized so a selection or detail change re-renders only the affected rows,
// not all up-to-500 of them.
const TrafficRow = memo(function TrafficRow({
  entry: e,
  isSelected,
  isActive,
  onRowClick,
  onCheckbox,
  sendToRepeater,
  sendTitle
}: TrafficRowProps) {
  return (
    <tr
      onClick={() => onRowClick(e.requestId)}
      style={{
        borderBottom: '1px solid var(--border)',
        background: isActive ? 'var(--row-active)' : isSelected ? 'var(--row-selected)' : undefined,
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
          title={sendTitle}
          style={{ fontSize: 10, padding: '1px 6px', lineHeight: 1.2 }}
        >
          ↻R
        </button>
      </td>
    </tr>
  )
})

const th: React.CSSProperties = {
  textAlign: 'left',
  padding: '6px 8px',
  borderBottom: '1px solid var(--border)',
  fontWeight: 'normal',
  opacity: 0.7
}

const td: React.CSSProperties = {
  padding: '4px 8px'
}

function statusColor(status?: number): string {
  if (!status) return 'var(--http-none)'
  if (status >= 500) return 'var(--http-5xx)'
  if (status >= 400) return 'var(--http-4xx)'
  if (status >= 300) return 'var(--http-3xx)'
  return 'var(--http-2xx)'
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n}B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)}KB`
  return `${(n / 1024 / 1024).toFixed(1)}MB`
}
