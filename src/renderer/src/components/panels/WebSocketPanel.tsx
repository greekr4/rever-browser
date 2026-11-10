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

/** Which sockets are still open in the page (or a cross-site iframe), refreshed every 2s. */
export function useLiveSockets(): Array<{ url: string; readyState: number; frame: string }> {
  const [liveSockets, setLiveSockets] = useState<Array<{ url: string; readyState: number; frame: string }>>([])
  useEffect(() => {
    const poll = async () => {
      const r = await window.rev.ws.sockets()
      if (Array.isArray(r)) setLiveSockets(r)
    }
    void poll()
    const id = setInterval(() => void poll(), 2000)
    return () => clearInterval(id)
  }, [])
  return liveSockets
}

export function WebSocketPanel() {
  const t = useT()
  const [connections, setConnections] = useState<WsConnection[]>([])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [frames, setFrames] = useState<WSFrame[]>([])
  const frameLastTsRef = useRef<number>(0)
  const bottomRef = useRef<HTMLDivElement>(null)

  // ── 송신: 페이지가 이미 연 소켓을 메모리에서 찾아 그 소켓으로 보낸다 ──
  const [payload, setPayload] = useState('')
  const [sending, setSending] = useState(false)
  const [sendResult, setSendResult] = useState<string | null>(null)
  const liveSockets = useLiveSockets()
  const payloadRef = useRef<HTMLTextAreaElement>(null)
  const selectedUrl = connections.find((c) => c.requestId === selectedId)?.url
  const live = liveSockets.find((w) => w.url === selectedUrl && w.readyState === 1)

  const sendOn = async (url: string, data: string, asBinary = false) => {
    setSending(true)
    try {
      // 열린 소켓이 없으면 페이지가 같은 URL로 다시 연 뒤 보낸다.
      const reconnect = !liveSockets.some((w) => w.url === url && w.readyState === 1)
      const r = await window.rev.ws.send(url, data, asBinary, reconnect)
      setSendResult(r.ok ? t(r.reconnected ? 'panel.wsSender.reconnectSent' : 'panel.wsSender.sent') : (r.error ?? 'error'))
      if (r.reconnected) {
        // 재연결은 새 연결(새 requestId)이다 — 응답이 보이도록 그 연결로 옮긴다. 목록은 최신순.
        const list = await window.rev.ws.list()
        setConnections(list.map((c) => ({ requestId: c.requestId, url: c.url, startedAt: c.startedAt })))
        setSelectedId(list.find((c) => c.url === url)?.requestId ?? selectedId)
      }
    } finally {
      setSending(false)
    }
  }

  const send = async () => {
    if (!selectedUrl || !payload || sending) return
    await sendOn(selectedUrl, payload)
  }

  // 재전송: 그 프레임이 오간 연결의 소켓으로 같은 내용을 다시 보낸다.
  const resend = async (frameUrl: string | undefined, f: WSFrame) => {
    if (!frameUrl) return
    await sendOn(frameUrl, f.payloadData, f.opcode === 2)
  }

  // 변조: 페이로드를 작성란에 올리고 포커스 — 수정 후 Send로 전송.
  const editFrame = (f: WSFrame) => {
    setPayload(f.payloadData)
    payloadRef.current?.focus()
  }

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
        // Scroll only the frame list; scrollIntoView would also move every ancestor.
        requestAnimationFrame(() => {
          const list = bottomRef.current?.parentElement
          if (list) list.scrollTop = list.scrollHeight
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
            {/* host + path + query: two sockets on one host (a page and its chat widget) must stay distinguishable */}
            {connLabel(c.url)}
          </div>
        ))}
      </div>

      {/* Right: frames + sender */}
      <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minHeight: 0 }}>
        {/* Toolbar on top, same as the other panels: which page socket to send through. */}
        <div className="panel-toolbar">
          <span style={{ color: 'var(--text-dim)', flexShrink: 0 }}>{t('panel.wsSender.target')}</span>
          <code title={selectedUrl} style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
            {selectedUrl ?? '—'}
          </code>
          {selectedUrl && (
            <span style={{ flexShrink: 0, color: live ? 'var(--status-ok)' : 'var(--text-dim)' }} data-testid="ws-live">
              {live ? (live.frame === 'page' ? t('panel.wsSender.live') : t('panel.wsSender.liveFrame')) : t('panel.wsSender.noLive')}
            </span>
          )}
          {sendResult && (
            <span title={sendResult} style={{ maxWidth: '35%', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', color: 'var(--text-dim)' }}>
              {sendResult}
            </span>
          )}
        </div>
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
                <span style={{ flex: 1, wordBreak: 'break-all', whiteSpace: 'pre-wrap', color: 'var(--text)' }}>
                  {payload}
                </span>
                <span style={{ flexShrink: 0, display: 'flex', gap: 2 }}>
                  <button
                    title={t('panel.wsSender.resend')}
                    onClick={() => void resend(selectedUrl, f)}
                    className="btn-ghost btn-icon"
                  >
                    ↻
                  </button>
                  <button
                    title={t('panel.wsSender.edit')}
                    onClick={() => editFrame(f)}
                    className="btn-ghost btn-icon"
                  >
                    ✎
                  </button>
                </span>
              </div>
            )
          })}
          <div ref={bottomRef} />
        </div>

        {/* Composer — like the Console input: Enter sends through the page's own socket (auth preserved) */}
        <div style={{ display: 'flex', borderTop: '1px solid var(--border)', background: 'var(--bg)' }}>
          <textarea
            ref={payloadRef}
            value={payload}
            onChange={(e) => setPayload(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault()
                if (selectedUrl && payload && !sending) void send()
              }
            }}
            placeholder={t('panel.wsSender.payload')}
            aria-label={t('panel.wsSender.payload')}
            spellCheck={false}
            rows={Math.min(6, payload.split('\n').length)}
            style={{
              flex: 1,
              minWidth: 0,
              resize: 'none',
              border: 'none',
              padding: '4px 10px',
              fontFamily: 'ui-monospace, monospace',
              fontSize: 11,
              background: 'transparent',
              color: 'var(--text)',
              outline: 'none'
            }}
          />
          <button
            onClick={() => void send()}
            disabled={!selectedUrl || !payload || sending}
            className="btn-primary"
            style={{ margin: 4, alignSelf: 'flex-end' }}
            data-testid="ws-send"
          >
            {sending ? t('panel.wsSender.sending') : t(live ? 'panel.wsSender.send' : 'panel.wsSender.reconnectSend')}
          </button>
        </div>
      </div>
    </div>
  )
}

export function connLabel(url: string): string {
  try {
    const u = new URL(url)
    return u.host + u.pathname + u.search
  } catch {
    return url
  }
}
