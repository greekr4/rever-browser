import { useEffect, useRef, useState, type CSSProperties } from 'react'

import { connLabel, useLiveSockets } from '@/components/panels/WebSocketPanel'
import { tryPretty } from '@/lib/format-json'
import { runSequence, type WsStep } from '@/lib/ws-sequence'
import { useT } from '@/stores/i18n'

interface WSFrame {
  direction: 'sent' | 'received'
  opcode: number
  payloadData: string
  timestamp: number
}

const MONO: CSSProperties = { fontFamily: 'ui-monospace, monospace', fontSize: 11 }
const HEAD: CSSProperties = { padding: '4px 8px', fontSize: 10, color: 'var(--text-dim)', borderBottom: '1px solid var(--border)', display: 'flex', alignItems: 'center', gap: 6, flexShrink: 0 }
const ROW: CSSProperties = { ...MONO, padding: '3px 8px', borderBottom: '1px solid var(--border)', wordBreak: 'break-all', whiteSpace: 'pre-wrap' }
const DEFAULT_DELAY_MS = 100

/**
 * WebSocket counterpart of the HTTP repeater: pick a frame the page sent, edit it,
 * send it on the page's own socket and watch what comes back. A sequence replays
 * several frames in order (reconnect → auth → subscribe).
 */
export function WsRepeaterPanel() {
  const t = useT()
  const [conns, setConns] = useState<Array<{ requestId: string; url: string }>>([])
  const [url, setUrl] = useState<string | null>(null)
  const [frames, setFrames] = useState<WSFrame[]>([])
  const [original, setOriginal] = useState<WSFrame | null>(null)
  const [draft, setDraft] = useState('')
  const [steps, setSteps] = useState<WsStep[]>([])
  const [sentAt, setSentAt] = useState<number | null>(null)
  const [busy, setBusy] = useState(false)
  const [status, setStatus] = useState<string | null>(null)
  const connsRef = useRef(conns)
  connsRef.current = conns

  const live = useLiveSockets().find((w) => w.url === url && w.readyState === 1)
  // A socket is addressed by URL: a reconnect is a new connection to the same one.
  const urls = [...new Set(conns.map((c) => c.url))]
  const asBinary = original?.opcode === 2

  useEffect(() => {
    const poll = async () => {
      const list = await window.rev.ws.list()
      setConns((prev) =>
        list.length === prev.length && list.every((r, i) => r.requestId === prev[i].requestId)
          ? prev
          : list.map((r) => ({ requestId: r.requestId, url: r.url }))
      )
    }
    void poll()
    const id = setInterval(() => void poll(), 1000)
    return () => clearInterval(id)
  }, [])

  useEffect(() => {
    if (!url || !urls.includes(url)) setUrl(urls[0] ?? null)
  }, [url, urls.join('\n')])

  // Frames of every connection to this URL, oldest first.
  useEffect(() => {
    if (!url) return
    setFrames([])
    let since = 0
    let running = false
    let cancelled = false
    const poll = async () => {
      if (running) return
      running = true
      try {
        const ids = connsRef.current.filter((c) => c.url === url).map((c) => c.requestId)
        const next = (await Promise.all(ids.map((id) => window.rev.ws.frames(id, since, 500)))).flat().sort((a, b) => a.timestamp - b.timestamp)
        if (cancelled || next.length === 0) return
        since = next[next.length - 1].timestamp + 1
        setFrames((prev) => [...prev, ...next].slice(-1000))
      } finally {
        running = false
      }
    }
    void poll()
    const id = setInterval(() => void poll(), 500)
    return () => {
      cancelled = true
      clearInterval(id)
    }
  }, [url])

  const pick = (next: string) => {
    setUrl(next)
    setOriginal(null)
    setSentAt(null)
    setStatus(null)
  }

  const load = (f: WSFrame) => {
    setOriginal(f)
    setDraft(f.payloadData)
  }

  const send = async () => {
    if (!url || !draft || busy) return
    setBusy(true)
    setSentAt(Date.now())
    try {
      const r = await window.rev.ws.send(url, draft, asBinary, !live)
      setStatus(r.ok ? t(r.reconnected ? 'panel.wsSender.reconnectSent' : 'panel.wsSender.sent') : (r.error ?? 'error'))
    } finally {
      setBusy(false)
    }
  }

  const play = async () => {
    if (!url || steps.length === 0 || busy) return
    setBusy(true)
    setSentAt(Date.now())
    try {
      const r = await runSequence(steps, (s, reconnect) => window.rev.ws.send(url, s.payload, s.asBinary, reconnect))
      setStatus(r.error ? t('wsRepeater.seqFailed', { n: r.sent + 1, error: r.error }) : t('wsRepeater.seqDone', { n: r.sent }))
    } finally {
      setBusy(false)
    }
  }

  if (urls.length === 0) return <div className="panel-empty">{t('panel.noWs')}</div>

  const sent = frames.filter((f) => f.direction === 'sent').slice(-200).reverse()
  const replies = sentAt == null ? [] : frames.filter((f) => f.direction === 'received' && f.timestamp >= sentAt)

  return (
    <div style={{ display: 'flex', height: '100%', minHeight: 0 }}>
      {/* Left: socket, the frames the page sent on it, the sequence */}
      <div style={{ width: 230, flexShrink: 0, borderRight: '1px solid var(--border)', display: 'flex', flexDirection: 'column', minHeight: 0 }}>
        <select value={url ?? ''} onChange={(e) => pick(e.target.value)} aria-label={t('panel.wsSender.target')} data-testid="wsr-socket" style={{ margin: 6, ...MONO }}>
          {urls.map((u) => (
            <option key={u} value={u}>
              {connLabel(u)}
            </option>
          ))}
        </select>
        <div style={HEAD}>{t('wsRepeater.sentFrames')}</div>
        <div style={{ flex: 1, minHeight: 0, overflowY: 'auto' }} data-testid="wsr-sent">
          {sent.length === 0 && <div className="panel-empty">{t('wsRepeater.noSent')}</div>}
          {sent.map((f, i) => (
            <div
              key={`${f.timestamp}-${i}`}
              onClick={() => load(f)}
              style={{ ...ROW, cursor: 'pointer', background: f === original ? 'var(--row-active)' : 'transparent', maxHeight: 42, overflow: 'hidden' }}
            >
              {f.payloadData.slice(0, 160)}
            </div>
          ))}
        </div>
        {steps.length > 0 && (
          <div style={{ maxHeight: '45%', display: 'flex', flexDirection: 'column', borderTop: '1px solid var(--border)' }} data-testid="wsr-sequence">
            <div style={HEAD}>
              <span style={{ flex: 1 }}>{t('wsRepeater.sequence')}</span>
              <button className="btn-primary" onClick={() => void play()} disabled={busy}>
                ▶ {t('wsRepeater.play')}
              </button>
            </div>
            <div style={{ overflowY: 'auto' }}>
              {steps.map((s, i) => (
                <div key={i} style={{ ...ROW, display: 'flex', gap: 4, alignItems: 'center', whiteSpace: 'nowrap' }}>
                  <input
                    type="number"
                    min={0}
                    value={s.delayMs}
                    onChange={(e) => setSteps((prev) => prev.map((p, j) => (j === i ? { ...p, delayMs: Math.max(0, Number(e.target.value) || 0) } : p)))}
                    title={t('wsRepeater.delay')}
                    aria-label={t('wsRepeater.delay')}
                    style={{ width: 52, ...MONO }}
                  />
                  <span title={s.payload} style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis' }}>
                    {s.payload}
                  </span>
                  <button className="btn-danger btn-icon" title={t('wsRepeater.removeStep')} onClick={() => setSteps((prev) => prev.filter((_, j) => j !== i))}>
                    ✕
                  </button>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* Middle: original on top, the edit below */}
      <div style={{ flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column', minHeight: 0 }}>
        <div className="panel-toolbar">
          <span style={{ color: 'var(--text-dim)', flexShrink: 0 }}>{t('panel.wsSender.target')}</span>
          <code title={url ?? ''} style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
            {url}
          </code>
          <span style={{ flexShrink: 0, color: live ? 'var(--status-ok)' : 'var(--text-dim)' }} data-testid="wsr-live">
            {live ? (live.frame === 'page' ? t('panel.wsSender.live') : t('panel.wsSender.liveFrame')) : t('panel.wsSender.noLive')}
          </span>
          {status && (
            <span title={status} data-testid="wsr-status" style={{ maxWidth: '40%', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', color: 'var(--text-dim)' }}>
              {status}
            </span>
          )}
        </div>
        {original && (
          <>
            <div style={HEAD}>
              {t('wsRepeater.original')}
              {asBinary && ' · base64'}
            </div>
            <div style={{ ...ROW, maxHeight: '30%', overflowY: 'auto', color: 'var(--text-dim)', flexShrink: 0 }}>{original.payloadData}</div>
          </>
        )}
        <textarea
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
              e.preventDefault()
              void send()
            }
          }}
          placeholder={t('panel.wsSender.payload')}
          aria-label={t('panel.wsSender.payload')}
          spellCheck={false}
          style={{ flex: 1, minHeight: 40, resize: 'none', border: 'none', padding: '6px 10px', background: 'transparent', color: 'var(--text)', outline: 'none', ...MONO }}
        />
        <div style={{ display: 'flex', gap: 4, padding: 4, borderTop: '1px solid var(--border)', alignItems: 'center' }}>
          <button className="btn-ghost" onClick={() => setDraft(tryPretty(draft))} disabled={!draft}>
            {t('wsRepeater.format')}
          </button>
          <button
            className="btn-ghost"
            onClick={() => setSteps((prev) => [...prev, { payload: draft, delayMs: DEFAULT_DELAY_MS, asBinary }])}
            disabled={!draft}
            data-testid="wsr-add-step"
          >
            {t('wsRepeater.addStep')}
          </button>
          <span style={{ flex: 1 }} />
          <button className="btn-primary" onClick={() => void send()} disabled={!draft || busy} data-testid="wsr-send">
            {busy ? t('panel.wsSender.sending') : t(live ? 'panel.wsSender.send' : 'panel.wsSender.reconnectSend')}
          </button>
        </div>
      </div>

      {/* Right: everything received since the send — a socket has no request/response ids to pair by */}
      <div style={{ width: '32%', flexShrink: 0, borderLeft: '1px solid var(--border)', display: 'flex', flexDirection: 'column', minHeight: 0 }}>
        <div style={HEAD}>{t('wsRepeater.responses')}</div>
        <div style={{ flex: 1, minHeight: 0, overflowY: 'auto' }} data-testid="wsr-replies">
          {replies.length === 0 && <div className="panel-empty">{t('wsRepeater.noResponses')}</div>}
          {replies.map((f, i) => (
            <div key={`${f.timestamp}-${i}`} style={{ ...ROW, display: 'flex', gap: 6 }}>
              <span style={{ flexShrink: 0, color: 'var(--status-ok)' }}>+{f.timestamp - sentAt!}ms</span>
              <span style={{ flex: 1, minWidth: 0 }}>{f.payloadData.length > 1024 ? f.payloadData.slice(0, 1024) + '…' : f.payloadData}</span>
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}
