import { useT } from '@/stores/i18n'

import { useEffect, useRef, useState } from 'react'

interface WsConnection {
  requestId: string
  url: string
  startedAt: number
}

interface WSFrame {
  direction: 'sent' | 'received'
  opcode: number
  payloadData: string
  timestamp: number
}

export function WebSocketPanel() {
  const t = useT()
  const [connections, setConnections] = useState<WsConnection[]>([])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [frames, setFrames] = useState<WSFrame[]>([])
  const frameLastTsRef = useRef<number>(0)
  const bottomRef = useRef<HTMLDivElement>(null)

  // Poll WS connection list
  useEffect(() => {
    const poll = async () => {
      const list = await window.rev.ws.list()
      setConnections((prev) => {
        const same =
          list.length === prev.length && list.every((r, i) => r.requestId === prev[i].requestId)
        if (same) return prev
        return list.map((r) => ({ requestId: r.requestId, url: r.url, startedAt: r.startedAt }))
      })
    }
    void poll()
    const id = setInterval(() => void poll(), 1000)
    return () => clearInterval(id)
  }, [])

  // Auto-select first connection
  useEffect(() => {
    const stillOpen = selectedId && connections.some((c) => c.requestId === selectedId)
    if (!stillOpen) setSelectedId(connections[0]?.requestId ?? null)
  }, [connections, selectedId])

  // Poll frames for selected connection
  useEffect(() => {
    if (!selectedId) return
    frameLastTsRef.current = 0
    setFrames([])

    const poll = async () => {
      const next = await window.rev.ws.frames(selectedId, frameLastTsRef.current)
      if (next.length > 0) {
        frameLastTsRef.current = next[next.length - 1].timestamp + 1
        setFrames((prev) => [...prev, ...next])
        requestAnimationFrame(() => {
          bottomRef.current?.scrollIntoView({ block: 'end' })
        })
      }
    }
    void poll()
    const id = setInterval(() => void poll(), 1000)
    return () => clearInterval(id)
  }, [selectedId])

  return (
    <div style={{ display: 'flex', height: '100%', minHeight: 0 }}>
      {/* Left: connection list */}
      <div
        style={{
          width: 180,
          flexShrink: 0,
          borderRight: '1px solid var(--border)',
          overflowY: 'auto',
          fontFamily: 'ui-monospace, monospace',
          fontSize: 11,
          scrollbarGutter: 'stable'
        }}
      >
        {connections.length === 0 && (
          <div className="panel-empty">{t('panel.noWs')}</div>
        )}
        {connections.map((c) => (
          <div
            key={c.requestId}
            onClick={() => setSelectedId(c.requestId)}
            style={{
              padding: '6px 8px',
              cursor: 'pointer',
              background: selectedId === c.requestId ? 'var(--row-active)' : 'transparent',
              borderBottom: '1px solid var(--border)',
              wordBreak: 'break-all'
            }}
          >
            {new URL(c.url).hostname}
          </div>
        ))}
      </div>

      {/* Right: frames */}
      <div style={{ flex: 1, overflowY: 'auto', fontFamily: 'ui-monospace, monospace', fontSize: 11, scrollbarGutter: 'stable' }}>
        {frames.length === 0 && selectedId && (
          <div className="panel-empty">{t('panel.noFrames')}</div>
        )}
        {!selectedId && (
          <div className="panel-empty">{t('panel.selectConnection')}</div>
        )}
        {frames.map((f, i) => {
          const payload = f.payloadData.length > 1024
            ? f.payloadData.slice(0, 1024) + '…'
            : f.payloadData
          return (
            <div
              key={i}
              style={{
                padding: '3px 8px',
                borderBottom: '1px solid var(--border)',
                display: 'flex',
                gap: 6,
                alignItems: 'flex-start'
              }}
            >
              <span
                style={{
                  flexShrink: 0,
                  color: f.direction === 'sent' ? 'var(--status-info)' : 'var(--status-ok)',
                  fontSize: 10
                }}
              >
                {f.direction === 'sent' ? '↑' : '↓'}
              </span>
              <span style={{ wordBreak: 'break-all', whiteSpace: 'pre-wrap', color: 'var(--text)' }}>
                {payload}
              </span>
            </div>
          )
        })}
        <div ref={bottomRef} />
      </div>
    </div>
  )
}
