import { useShallow } from 'zustand/react/shallow'

import { useTrafficStore } from '@/stores/traffic'

export function TrafficList() {
  const { entries, order, clear } = useTrafficStore(
    useShallow((s) => ({ entries: s.entries, order: s.order, clear: s.clear }))
  )
  const list = order.map((id) => entries[id]).filter(Boolean)

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <header
        style={{
          padding: '8px 12px',
          borderBottom: '1px solid #333',
          display: 'flex',
          gap: 8,
          alignItems: 'center'
        }}
      >
        <strong>Traffic ({list.length})</strong>
        <button onClick={clear} style={{ marginLeft: 'auto', fontSize: 11 }}>
          Clear
        </button>
      </header>
      <div style={{ flex: 1, overflowY: 'auto', fontFamily: 'ui-monospace, monospace', fontSize: 11 }}>
        {list.length === 0 && (
          <p style={{ padding: 12, opacity: 0.5 }}>
            브라우저를 시작하고 페이지를 열면 트래픽이 여기에 나타납니다.
          </p>
        )}
        <table style={{ width: '100%', borderCollapse: 'collapse' }}>
          <thead style={{ position: 'sticky', top: 0, background: '#1a1a1a' }}>
            <tr>
              <th style={th}>Method</th>
              <th style={th}>Status</th>
              <th style={th}>Type</th>
              <th style={{ ...th, width: '100%' }}>URL</th>
              <th style={th}>Size</th>
            </tr>
          </thead>
          <tbody>
            {list.map((e) => (
              <tr key={e.requestId} style={{ borderBottom: '1px solid #1f1f1f' }}>
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
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}

const th: React.CSSProperties = {
  textAlign: 'left',
  padding: '6px 8px',
  borderBottom: '1px solid #333',
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
