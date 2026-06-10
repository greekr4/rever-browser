import { useMemo } from 'react'
import { useShallow } from 'zustand/react/shallow'

import { useRepeaterStore, type RepeaterRequestSpec } from '@/stores/repeater'

const METHODS = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS']

export function RepeaterPanel() {
  const {
    active,
    sourceLabel,
    history,
    loading,
    error,
    setActive,
    send,
    restoreFromHistory,
    clear
  } = useRepeaterStore(
    useShallow((s) => ({
      active: s.active,
      sourceLabel: s.sourceLabel,
      history: s.history,
      loading: s.loading,
      error: s.error,
      setActive: s.setActive,
      send: s.send,
      restoreFromHistory: s.restoreFromHistory,
      clear: s.clear
    }))
  )

  if (!active) {
    return (
      <div style={{ padding: 20, opacity: 0.6, fontSize: 12 }}>
        Right-click a row in Traffic and choose <strong>Send to Repeater</strong> to start.
      </div>
    )
  }

  const update = (patch: Partial<RepeaterRequestSpec>) => setActive({ ...active, ...patch })

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', minHeight: 0 }}>
      <div
        style={{
          padding: '8px 12px',
          borderBottom: '1px solid #2a2a2a',
          display: 'flex',
          gap: 8,
          alignItems: 'center',
          fontSize: 11,
          flexShrink: 0
        }}
      >
        <span style={{ opacity: 0.6 }}>source:</span>
        <span
          style={{
            fontFamily: 'ui-monospace, monospace',
            opacity: 0.8,
            whiteSpace: 'nowrap',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            flex: 1
          }}
          title={sourceLabel ?? ''}
        >
          {sourceLabel}
        </span>
        <button onClick={() => void send()} disabled={loading} style={{ fontSize: 11 }}>
          {loading ? 'Sending…' : 'Send'}
        </button>
        <button onClick={clear} style={{ fontSize: 11 }}>
          Clear
        </button>
      </div>

      <div style={{ flex: 1, display: 'flex', minHeight: 0 }}>
        <div
          style={{
            flex: 1,
            display: 'flex',
            flexDirection: 'column',
            borderRight: '1px solid #2a2a2a',
            minWidth: 0,
            minHeight: 0
          }}
        >
          <RequestEditor active={active} onChange={update} />
        </div>
        <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minWidth: 0, minHeight: 0 }}>
          <ResponseViewer history={history} error={error} loading={loading} />
        </div>
      </div>

      {history.length > 0 && (
        <HistoryStrip
          history={history}
          onRestore={restoreFromHistory}
        />
      )}
    </div>
  )
}

interface EditorProps {
  active: RepeaterRequestSpec
  onChange: (patch: Partial<RepeaterRequestSpec>) => void
}

function RequestEditor({ active, onChange }: EditorProps) {
  // 인덱스 기반 key를 유지하기 위해 [key, value] 쌍 배열로 관리
  const headerEntries = useMemo(() => Object.entries(active.headers), [active.headers])

  // entries 배열 → headers 객체 변환 헬퍼
  const entriesToHeaders = (entries: [string, string][]): Record<string, string> => {
    const result: Record<string, string> = {}
    for (const [k, v] of entries) {
      if (k) result[k] = v
    }
    return result
  }

  const setHeaderKey = (idx: number, newKey: string) => {
    const next = headerEntries.map((pair, i) =>
      i === idx ? ([newKey, pair[1]] as [string, string]) : pair
    )
    onChange({ headers: entriesToHeaders(next) })
  }
  const setHeaderValue = (idx: number, value: string) => {
    const next = headerEntries.map((pair, i) =>
      i === idx ? ([pair[0], value] as [string, string]) : pair
    )
    onChange({ headers: entriesToHeaders(next) })
  }
  const removeHeader = (idx: number) => {
    const next = headerEntries.filter((_, i) => i !== idx)
    onChange({ headers: entriesToHeaders(next) })
  }
  const addHeader = () => {
    let name = 'X-New-Header'
    let i = 1
    while (name in active.headers) name = `X-New-Header-${i++}`
    onChange({ headers: { ...active.headers, [name]: '' } })
  }

  return (
    <>
      <div
        style={{
          padding: '6px 12px',
          fontSize: 10,
          opacity: 0.6,
          textTransform: 'uppercase',
          letterSpacing: 0.5,
          borderBottom: '1px solid #1f1f1f'
        }}
      >
        Request
      </div>
      <div style={{ padding: 10, display: 'flex', gap: 6, alignItems: 'center', flexShrink: 0 }}>
        <select
          value={active.method}
          onChange={(e) => onChange({ method: e.target.value })}
          style={{ padding: '4px 6px', fontSize: 11 }}
        >
          {METHODS.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
        <input
          value={active.url}
          onChange={(e) => onChange({ url: e.target.value })}
          spellCheck={false}
          style={{
            flex: 1,
            padding: '4px 8px',
            fontSize: 11,
            fontFamily: 'ui-monospace, monospace'
          }}
        />
      </div>

      <div style={{ flex: 1, overflow: 'auto', padding: '0 10px 10px', minHeight: 0 }}>
        <div
          style={{
            fontSize: 10,
            opacity: 0.6,
            textTransform: 'uppercase',
            letterSpacing: 0.5,
            margin: '8px 0 4px',
            display: 'flex',
            alignItems: 'center'
          }}
        >
          Headers ({headerEntries.length})
          <button onClick={addHeader} style={{ marginLeft: 'auto', fontSize: 10 }}>
            + add
          </button>
        </div>
        <table
          style={{
            width: '100%',
            borderCollapse: 'collapse',
            fontFamily: 'ui-monospace, monospace',
            fontSize: 11
          }}
        >
          <tbody>
            {headerEntries.map(([k, v], idx) => (
              <tr key={idx}>
                <td style={{ padding: 1, width: '40%' }}>
                  <input
                    value={k}
                    onChange={(e) => setHeaderKey(idx, e.target.value)}
                    spellCheck={false}
                    style={{
                      width: '100%',
                      padding: '2px 6px',
                      fontSize: 11,
                      fontFamily: 'inherit'
                    }}
                  />
                </td>
                <td style={{ padding: 1 }}>
                  <input
                    value={v}
                    onChange={(e) => setHeaderValue(idx, e.target.value)}
                    spellCheck={false}
                    style={{
                      width: '100%',
                      padding: '2px 6px',
                      fontSize: 11,
                      fontFamily: 'inherit'
                    }}
                  />
                </td>
                <td style={{ padding: 1, width: 24 }}>
                  <button
                    onClick={() => removeHeader(idx)}
                    title="Remove header"
                    style={{ padding: '2px 6px', fontSize: 11, lineHeight: 1 }}
                  >
                    ×
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>

        <div
          style={{
            fontSize: 10,
            opacity: 0.6,
            textTransform: 'uppercase',
            letterSpacing: 0.5,
            margin: '12px 0 4px'
          }}
        >
          Body
        </div>
        <textarea
          value={active.body ?? ''}
          onChange={(e) => onChange({ body: e.target.value || undefined })}
          placeholder={
            active.method === 'GET' || active.method === 'HEAD'
              ? '(no body for GET/HEAD)'
              : 'Request body…'
          }
          spellCheck={false}
          style={{
            width: '100%',
            minHeight: 120,
            padding: 8,
            background: '#0c0c0c',
            border: '1px solid #1f1f1f',
            borderRadius: 4,
            color: '#e6e6e6',
            fontFamily: 'ui-monospace, monospace',
            fontSize: 11,
            resize: 'vertical',
            boxSizing: 'border-box'
          }}
        />
      </div>
    </>
  )
}

interface ResponseProps {
  history: ReturnType<typeof useRepeaterStore.getState>['history']
  loading: boolean
  error: string | null
}

function ResponseViewer({ history, loading, error }: ResponseProps) {
  const latest = history[0]
  return (
    <>
      <div
        style={{
          padding: '6px 12px',
          fontSize: 10,
          opacity: 0.6,
          textTransform: 'uppercase',
          letterSpacing: 0.5,
          borderBottom: '1px solid #1f1f1f'
        }}
      >
        Response
      </div>
      <div style={{ flex: 1, overflow: 'auto', padding: 10, minHeight: 0 }}>
        {loading && <div style={{ opacity: 0.6, fontSize: 11 }}>Sending…</div>}
        {!loading && !latest && !error && (
          <div style={{ opacity: 0.4, fontSize: 11 }}>No response yet — press Send.</div>
        )}
        {error && !loading && (
          <div style={{ color: '#f88', fontSize: 11, marginBottom: 8, fontFamily: 'ui-monospace, monospace' }}>
            error: {error}
          </div>
        )}
        {latest && !loading && (
          <div style={{ fontFamily: 'ui-monospace, monospace', fontSize: 11 }}>
            <div style={{ marginBottom: 8 }}>
              <strong style={{ color: statusColor(latest.response.status) }}>
                {latest.response.status || '—'}
              </strong>{' '}
              {latest.response.statusText}
              <span style={{ opacity: 0.6, marginLeft: 10 }}>
                {latest.response.timeMs}ms · {formatBytes(latest.response.bodyByteLength)}
              </span>
            </div>
            <details style={{ marginBottom: 8 }}>
              <summary style={{ cursor: 'pointer', opacity: 0.7, fontSize: 11 }}>
                Headers ({Object.keys(latest.response.headers).length})
              </summary>
              <pre
                style={{
                  margin: '4px 0 0',
                  padding: 8,
                  background: '#0c0c0c',
                  border: '1px solid #1f1f1f',
                  borderRadius: 4,
                  whiteSpace: 'pre-wrap',
                  wordBreak: 'break-all'
                }}
              >
                {Object.entries(latest.response.headers)
                  .map(([k, v]) => `${k}: ${v}`)
                  .join('\n')}
              </pre>
            </details>
            <pre
              style={{
                margin: 0,
                padding: 8,
                background: '#0c0c0c',
                border: '1px solid #1f1f1f',
                borderRadius: 4,
                whiteSpace: 'pre-wrap',
                wordBreak: 'break-all'
              }}
            >
              {tryPretty(latest.response.body, latest.response.headers['content-type'])}
              {latest.response.bodyTruncated && '\n\n[truncated…]'}
            </pre>
          </div>
        )}
      </div>
    </>
  )
}

interface HistoryStripProps {
  history: ReturnType<typeof useRepeaterStore.getState>['history']
  onRestore: (idx: number) => void
}

function HistoryStrip({ history, onRestore }: HistoryStripProps) {
  return (
    <div
      style={{
        borderTop: '1px solid #2a2a2a',
        padding: '6px 10px',
        maxHeight: 110,
        overflow: 'auto',
        fontSize: 11,
        flexShrink: 0
      }}
    >
      <div style={{ fontSize: 10, opacity: 0.6, textTransform: 'uppercase', letterSpacing: 0.5, marginBottom: 4 }}>
        History ({history.length})
      </div>
      {history.map((h, i) => (
        <div
          key={h.ts}
          style={{
            display: 'flex',
            gap: 8,
            alignItems: 'center',
            padding: '2px 0',
            fontFamily: 'ui-monospace, monospace'
          }}
        >
          <span style={{ color: statusColor(h.response.status), width: 38 }}>
            {h.response.status || 'ERR'}
          </span>
          <span style={{ opacity: 0.7, width: 60 }}>{h.response.timeMs}ms</span>
          <span style={{ opacity: 0.6, width: 70 }}>{h.request.method}</span>
          <span
            style={{
              flex: 1,
              opacity: 0.7,
              whiteSpace: 'nowrap',
              overflow: 'hidden',
              textOverflow: 'ellipsis'
            }}
            title={h.request.url}
          >
            {h.request.url}
          </span>
          <button onClick={() => onRestore(i)} style={{ fontSize: 10 }}>
            Restore
          </button>
        </div>
      ))}
    </div>
  )
}

function statusColor(status: number): string {
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

function tryPretty(body: string, contentType?: string): string {
  if (!body) return '(empty)'
  if (contentType && contentType.includes('json')) {
    try {
      return JSON.stringify(JSON.parse(body), null, 2)
    } catch {
      return body
    }
  }
  return body
}
