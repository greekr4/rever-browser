import { useT } from '@/stores/i18n'
import { matchCaptureForUrl } from '../../../../shared/ws-match'

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

interface WsCapture {
  id: number
  url: string
  openedAt: number
  readyState: number
}

export function WebSocketPanel() {
  const t = useT()
  const [connections, setConnections] = useState<WsConnection[]>([])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [frames, setFrames] = useState<WSFrame[]>([])
  const frameLastTsRef = useRef<number>(0)
  const bottomRef = useRef<HTMLDivElement>(null)

  // ── 송신 (페이지 소켓 재사용) ──
  const [captures, setCaptures] = useState<WsCapture[]>([])
  const [captureId, setCaptureId] = useState<number | null>(null)
  const [payload, setPayload] = useState('')
  const [sending, setSending] = useState(false)
  const [sendResult, setSendResult] = useState<string | null>(null)
  const payloadRef = useRef<HTMLTextAreaElement>(null)
  const selectedUrl = connections.find((c) => c.requestId === selectedId)?.url

  // Poll page-socket captures for the sender bar
  useEffect(() => {
    const poll = async () => {
      const r = await window.rev.ws.captureList()
      if (!Array.isArray(r)) return
      setCaptures(r)
      setCaptureId((prev) => (prev != null && r.some((c) => c.id === prev) ? prev : r.find((c) => c.readyState === 1)?.id ?? r[0]?.id ?? null))
    }
    void poll()
    const id = setInterval(() => void poll(), 2000)
    return () => clearInterval(id)
  }, [])

  const enableCapture = async () => {
    const r = await window.rev.ws.captureEnable()
    setSendResult(r.ok ? t('panel.wsSender.installed') : (r.error ?? 'error'))
  }

  const send = async () => {
    if (captureId == null || !payload || sending) return
    setSending(true)
    try {
      const r = await window.rev.ws.send(captureId, payload)
      setSendResult(r.ok ? t('panel.wsSender.sent') : (r.error ?? 'error'))
    } finally {
      setSending(false)
    }
  }

  // 리필레이: 프레임의 연결 URL과 같은 열린 페이지 소켓을 찾아 보낸다.
  const resend = async (frameUrl: string | undefined, f: WSFrame) => {
    const cap = matchCaptureForUrl(captures, frameUrl ?? '')
    if (!cap) {
      setPayload(f.payloadData)
      setSendResult(t('panel.wsSender.noSocketMatch'))
      return
    }
    const r = await window.rev.ws.send(cap.id, f.payloadData, f.opcode === 2)
    setSendResult(r.ok ? t('panel.wsSender.sent') : (r.error ?? 'error'))
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

      {/* Right: frames + sender */}
      <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minHeight: 0 }}>
        {/* Toolbar on top, same as the other panels: which page socket to send through. */}
        <div className="panel-toolbar">
          <select
            value={captureId ?? ''}
            onChange={(e) => setCaptureId(e.target.value === '' ? null : Number(e.target.value))}
            aria-label={t('panel.wsSender.title')}
            style={{ flex: 1, minWidth: 0, fontSize: 11, padding: '2px 4px' }}
          >
            {captures.length === 0 && <option value="">{t('panel.wsSender.noCaptures')}</option>}
            {captures.map((c) => (
              <option key={c.id} value={c.id}>
                #{c.id} {safeHost(c.url)} ({c.readyState === 1 ? t('panel.wsSender.open') : `${t('panel.wsSender.state')} ${c.readyState}`})
              </option>
            ))}
          </select>
          {sendResult && (
            <span title={sendResult} style={{ maxWidth: '40%', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', color: 'var(--text-dim)' }}>
              {sendResult}
            </span>
          )}
          <button onClick={() => void enableCapture()} style={{ whiteSpace: 'nowrap' }}>
            {t('panel.wsSender.enable')}
          </button>
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
                if (captureId != null && payload && !sending) void send()
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
            disabled={captureId == null || !payload || sending}
            className="btn-primary"
            style={{ margin: 4, alignSelf: 'flex-end' }}
          >
            {sending ? t('panel.wsSender.sending') : t('panel.wsSender.send')}
          </button>
        </div>
      </div>
    </div>
  )
}

function safeHost(url: string): string {
  try {
    return new URL(url).hostname
  } catch {
    return url
  }
}
