import { useEffect, useRef, useState } from 'react'

import { ProviderIcon } from '@/components/chat/ProviderIcon'
import { Markdown } from '@/components/chat/Markdown'
import { useChatDraft } from '@/stores/chat-draft'
import { useT } from '@/stores/i18n'
import { useScanStore, type LogEntry } from '@/stores/scan'
import { useTabsStore } from '@/stores/tabs'

// Movable Red Team popup. State lives in useScanStore, so closing this popup
// mid-scan keeps the run and its report alive — reopening shows it live.
export function ScanPopup() {
  const tr = useT()
  const s = useScanStore()
  const tabs = useTabsStore((st) => st.tabs)
  const activeUrl = useTabsStore((st) => st.tabs.find((t) => t.id === st.activeId)?.url ?? '')

  const [pos, setPos] = useState({ x: 0, y: 0 })
  const [placed, setPlaced] = useState(false)
  const drag = useRef<{ dx: number; dy: number } | null>(null)
  const [, tick] = useState(0)

  // Center on first open; seed the target from the active tab if empty.
  useEffect(() => {
    if (!s.visible) return
    if (!placed) {
      setPos({ x: Math.max(20, window.innerWidth / 2 - 310), y: 90 })
      setPlaced(true)
    }
    if (!s.target) s.setTarget(activeUrl)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [s.visible])

  // Live elapsed clock while running.
  useEffect(() => {
    if (s.phase !== 'running') return
    const id = setInterval(() => tick((n) => n + 1), 1000)
    return () => clearInterval(id)
  }, [s.phase])

  useEffect(() => {
    const move = (e: MouseEvent) => {
      if (!drag.current) return
      setPos({ x: e.clientX - drag.current.dx, y: e.clientY - drag.current.dy })
    }
    const up = () => (drag.current = null)
    window.addEventListener('mousemove', move)
    window.addEventListener('mouseup', up)
    return () => {
      window.removeEventListener('mousemove', move)
      window.removeEventListener('mouseup', up)
    }
  }, [])

  // Hooks must be above the early return — all hooks run every render.
  const [copied, setCopied] = useState(false)
  const [logOpen, setLogOpen] = useState(true)
  const logRef = useRef<HTMLDivElement>(null)

  // Auto-scroll the log to the newest line, but only when the user is already
  // near the bottom — so scrolling up to read history isn't yanked back down.
  useEffect(() => {
    const el = logRef.current
    if (!el) return
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 60) el.scrollTop = el.scrollHeight
  }, [s.log.length])

  if (!s.visible) return null
  const elapsed = s.phase === 'running' ? Math.floor((Date.now() - s.startedAt) / 1000) : 0
  const idle = s.phase === 'idle' || s.phase === 'done' || s.phase === 'error'

  const copyReport = () => {
    void navigator.clipboard.writeText(s.report).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }
  const saveReport = () => {
    const host = (() => {
      try {
        return new URL(s.target).hostname
      } catch {
        return 'site'
      }
    })()
    const blob = new Blob([s.report], { type: 'text/markdown' })
    const a = document.createElement('a')
    a.href = URL.createObjectURL(blob)
    a.download = `redteam-${host}-${new Date().toISOString().slice(0, 10)}.md`
    a.click()
    URL.revokeObjectURL(a.href)
  }
  const reportBtn: React.CSSProperties = {
    fontSize: 11,
    padding: '3px 10px',
    borderRadius: 4,
    border: '1px solid var(--border-2)',
    background: 'transparent',
    color: 'var(--text)',
    cursor: 'pointer'
  }

  const logColor = (kind: string): string =>
    kind === 'finding'
      ? 'var(--status-ok, #5fd07a)'
      : kind === 'error'
        ? 'var(--status-error)'
        : kind === 'phase'
          ? 'var(--chip-info-text)'
          : 'var(--text-dim)'
  const logGlyph = (kind: string): string =>
    kind === 'finding' ? '✓' : kind === 'error' ? '✕' : kind === 'phase' ? '»' : '›'
  const fmtAt = (ts: number): string => {
    const sec = Math.max(0, Math.floor((ts - (s.startedAt || ts)) / 1000))
    return `${String(Math.floor(sec / 60)).padStart(2, '0')}:${String(sec % 60).padStart(2, '0')}`
  }
  // A tool/finding line shows which tab fired it ([tab3]) and that tab's live
  // page (host + path, e.g. localhost:8080/analysis) so the user can see where.
  const tabInfo = (tabId?: string): { tag: string; loc: string } => {
    if (!tabId) return { tag: '', loc: '' }
    const idx = tabs.findIndex((t) => t.id === tabId)
    const url = (idx >= 0 ? tabs[idx]?.url : undefined) ?? s.scanTabs[tabId]?.url
    let loc = ''
    try {
      const u = new URL(url ?? '')
      loc = u.host + (u.pathname === '/' ? '' : u.pathname)
    } catch {}
    return { tag: idx >= 0 ? `[tab${idx + 1}]` : '', loc }
  }
  // Phase lines carry an i18n key translated now; literals (tool/error/finding
  // title) render as-is. An empty finding title falls back to a localized label.
  const lineText = (e: LogEntry): string =>
    e.key ? tr(e.key, e.vars) : e.text || (e.kind === 'finding' ? tr('scan.log.finding') : '')
  // Each intensity's risk shown as a colored dot + word, in place of prose.
  const risk = (level: string): { label: string; color: string } =>
    level === 'readonly'
      ? { label: tr('scan.risk.safe'), color: 'var(--status-ok, #5fd07a)' }
      : level === 'probe'
        ? { label: tr('scan.risk.caution'), color: '#e0a33e' }
        : { label: tr('scan.risk.danger'), color: 'var(--status-error)' }
  const riskDot = (color: string, size = 7): React.CSSProperties => ({
    display: 'inline-block',
    width: size,
    height: size,
    borderRadius: size,
    background: color,
    flexShrink: 0
  })

  return (
    <div
      data-testid="red-team-popup"
      style={{
        position: 'fixed',
        left: pos.x,
        top: pos.y,
        width: 620,
        maxHeight: '72vh',
        display: 'flex',
        flexDirection: 'column',
        background: 'var(--bg-panel, var(--bg))',
        border: '1px solid var(--border)',
        borderRadius: 8,
        boxShadow: '0 8px 32px rgba(0,0,0,0.45)',
        zIndex: 1000,
        overflow: 'hidden',
        fontSize: 12,
        color: 'var(--text)'
      }}
    >
      {/* Drag handle / header */}
      <div
        onMouseDown={(e) => (drag.current = { dx: e.clientX - pos.x, dy: e.clientY - pos.y })}
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 8,
          padding: '8px 12px',
          background: 'var(--bg-bar)',
          borderBottom: '1px solid var(--border)',
          cursor: 'move',
          userSelect: 'none'
        }}
      >
        <span style={{ fontWeight: 600 }}>🛡 Red Team</span>
        <span style={{ display: 'flex', alignItems: 'center', gap: 4, fontSize: 11, color: 'var(--text-dim)' }}>
          <ProviderIcon agentId="claude-code" size={12} /> Anthropic · Claude Code
        </span>
        {s.phase === 'running' && <span style={{ color: 'var(--accent-text)', fontSize: 11 }}>{tr('scan.panel.running')}</span>}
        <button
          onClick={() => s.close()}
          title={tr('scan.panel.close')}
          style={{
            marginLeft: 'auto',
            border: 'none',
            background: 'transparent',
            color: 'var(--text-dim)',
            cursor: 'pointer',
            fontSize: 14
          }}
        >
          ✕
        </button>
      </div>

      <div style={{ padding: 12, overflowY: 'auto' }}>
        {idle && (
          <>
            <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
              <span style={{ color: 'var(--text-dim)' }}>{tr('scan.panel.target')}</span>
              <input
                data-testid="scan-url"
                value={s.target}
                onChange={(e) => s.setTarget(e.target.value)}
                placeholder={tr('scan.bar.placeholder')}
                style={{
                  flex: 1,
                  minWidth: 240,
                  fontSize: 12,
                  padding: '4px 8px',
                  borderRadius: 4,
                  border: '1px solid var(--border-2)',
                  background: 'var(--bg)',
                  color: 'var(--text)',
                  fontFamily: 'ui-monospace, monospace'
                }}
              />
              <span style={{ color: 'var(--text-dim)' }}>{tr('scan.bar.pages')}</span>
              <input
                type="number"
                min={1}
                max={200}
                value={s.pages}
                onChange={(e) => s.setPages(Number(e.target.value))}
                style={{
                  width: 54,
                  fontSize: 12,
                  padding: '4px 6px',
                  borderRadius: 4,
                  border: '1px solid var(--border-2)',
                  background: 'var(--bg)',
                  color: 'var(--text)'
                }}
              />
              <button
                type="button"
                data-testid="scan-start"
                disabled={!s.target.trim()}
                onClick={() => s.askConfirm()}
                style={{
                  fontSize: 12,
                  fontWeight: 600,
                  padding: '4px 12px',
                  borderRadius: 4,
                  border: '1px solid var(--accent-border)',
                  background: s.target.trim() ? 'var(--accent-soft)' : 'transparent',
                  color: 'var(--chip-info-text)',
                  cursor: s.target.trim() ? 'pointer' : 'default'
                }}
              >
                ▶ {tr('scan.panel.start')}
              </button>
            </div>
            <div style={{ display: 'flex', gap: 6, alignItems: 'center', marginTop: 8, flexWrap: 'wrap' }}>
              <span style={{ color: 'var(--text-dim)', fontSize: 11 }}>{tr('scan.panel.intensity')}</span>
              {(['readonly', 'probe', 'redteam'] as const).map((level) => (
                <button
                  key={level}
                  type="button"
                  data-testid={`scan-intensity-${level}`}
                  onClick={() => s.setIntensity(level)}
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    gap: 6,
                    fontSize: 11,
                    padding: '3px 10px',
                    borderRadius: 4,
                    border: `1px solid ${s.intensity === level ? 'var(--accent-border)' : 'var(--border-2)'}`,
                    background: s.intensity === level ? 'var(--accent-soft)' : 'transparent',
                    color: s.intensity === level ? 'var(--chip-info-text)' : 'var(--text-dim)',
                    cursor: 'pointer'
                  }}
                >
                  <span style={riskDot(risk(level).color)} />
                  {tr(`scan.panel.intensity.${level}`)}
                </button>
              ))}
            </div>
            <div style={{ marginTop: 10, display: 'flex', alignItems: 'center', gap: 6, fontSize: 11 }}>
              <span style={{ color: 'var(--text-dim)' }}>{tr('scan.panel.risk')}</span>
              <span style={riskDot(risk(s.intensity).color, 8)} />
              <span style={{ color: risk(s.intensity).color, fontWeight: 600 }}>
                {risk(s.intensity).label}
              </span>
            </div>
          </>
        )}

        {s.phase === 'confirm' && (
          <div data-testid="scan-confirm">
            <div style={{ marginBottom: 8, wordBreak: 'break-all' }}>
              {s.intensity === 'readonly'
                ? tr('scan.panel.confirm', { url: s.target.trim(), n: String(s.pages) })
                : tr('scan.panel.confirm.probe', { url: s.target.trim(), n: String(s.pages), level: tr(`scan.panel.intensity.${s.intensity}`) })}
            </div>
            <div style={{ marginBottom: 12, display: 'flex', alignItems: 'center', gap: 6, fontSize: 11 }}>
              <span style={{ color: 'var(--text-dim)' }}>{tr('scan.panel.risk')}</span>
              <span style={riskDot(risk(s.intensity).color, 8)} />
              <span style={{ color: risk(s.intensity).color, fontWeight: 600 }}>
                {risk(s.intensity).label}
              </span>
            </div>
            <div style={{ display: 'flex', gap: 8 }}>
              <button
                type="button"
                data-testid="scan-confirm-yes"
                onClick={() => void s.start()}
                style={{
                  fontSize: 12,
                  fontWeight: 600,
                  padding: '5px 16px',
                  borderRadius: 4,
                  border: '1px solid var(--accent-border)',
                  background: 'var(--accent-soft)',
                  color: 'var(--chip-info-text)',
                  cursor: 'pointer'
                }}
              >
                {tr('scan.panel.yes')}
              </button>
              <button
                type="button"
                onClick={() => s.cancel()}
                style={{
                  fontSize: 12,
                  padding: '5px 16px',
                  borderRadius: 4,
                  border: '1px solid var(--border-2)',
                  background: 'transparent',
                  color: 'var(--text)',
                  cursor: 'pointer'
                }}
              >
                {tr('scan.panel.no')}
              </button>
            </div>
          </div>
        )}

        {s.phase === 'running' && (
          <div>
            <div style={{ display: 'flex', gap: 14, alignItems: 'center', marginBottom: 8, flexWrap: 'wrap' }}>
              <span>
                {tr('scan.panel.pages')} <b>{s.pagesSeen}</b>/{s.pages}
              </span>
              <span>
                {tr('scan.panel.findings')} <b>{s.findings}</b>
              </span>
              <span style={{ color: 'var(--text-dim)' }}>{elapsed}s</span>
              <button
                type="button"
                onClick={() => s.stop()}
                style={{
                  marginLeft: 'auto',
                  fontSize: 11,
                  padding: '3px 10px',
                  borderRadius: 4,
                  border: '1px solid var(--border-2)',
                  background: 'transparent',
                  color: 'var(--text)',
                  cursor: 'pointer'
                }}
              >
                ■ {tr('scan.panel.stop')}
              </button>
            </div>
          </div>
        )}

        {s.phase === 'error' && (
          <pre style={{ color: 'var(--status-error)', whiteSpace: 'pre-wrap' }}>{s.errorText}</pre>
        )}

        {s.phase === 'done' && s.report && (
          <div style={{ marginTop: 10 }}>
            <div style={{ display: 'flex', gap: 6, marginBottom: 8 }}>
              <button type="button" onClick={copyReport} style={reportBtn}>
                {copied ? '✓ ' : ''}
                {tr('scan.panel.copy')}
              </button>
              <button type="button" onClick={saveReport} style={reportBtn}>
                {tr('scan.panel.save')}
              </button>
              <button
                type="button"
                onClick={() => {
                  useChatDraft.getState().push(s.report)
                  s.close()
                }}
                style={reportBtn}
              >
                {tr('scan.panel.sendChat')}
              </button>
            </div>
            <div
              data-testid="scan-report"
              style={{
                padding: '2px 12px',
                background: 'var(--bg-bar)',
                border: '1px solid var(--border)',
                borderRadius: 6,
                fontSize: 12,
                lineHeight: 1.6
              }}
            >
              <Markdown text={s.report} />
            </div>
          </div>
        )}

        {s.log.length > 0 && s.phase !== 'idle' && s.phase !== 'confirm' && (
          <div style={{ marginTop: 10 }}>
            <button
              type="button"
              onClick={() => setLogOpen((v) => !v)}
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 6,
                width: '100%',
                padding: '4px 2px',
                background: 'transparent',
                border: 'none',
                color: 'var(--text-dim)',
                cursor: 'pointer',
                fontSize: 11,
                fontWeight: 600
              }}
            >
              <span>{logOpen ? '▾' : '▸'}</span>
              <span>{tr('scan.panel.log')}</span>
              <span style={{ opacity: 0.6 }}>({s.log.length})</span>
            </button>
            {logOpen && (
              <div
                ref={logRef}
                data-testid="scan-log"
                style={{
                  height: 170,
                  overflowY: 'auto',
                  background: '#0b0d10',
                  border: '1px solid var(--border)',
                  borderRadius: 6,
                  padding: '6px 8px',
                  fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
                  fontSize: 11,
                  lineHeight: 1.55
                }}
              >
                {s.log.map((e, i) => {
                  const info = e.kind === 'phase' ? { tag: '', loc: '' } : tabInfo(e.tabId)
                  return (
                    <div key={i} style={{ display: 'flex', gap: 6, whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>
                      <span style={{ color: '#555', flexShrink: 0 }}>{fmtAt(e.ts)}</span>
                      <span style={{ color: logColor(e.kind), flexShrink: 0 }}>{logGlyph(e.kind)}</span>
                      <span style={{ color: e.kind === 'tool' ? 'var(--text-2)' : logColor(e.kind), flex: 1, minWidth: 0 }}>
                        {info.tag && <span style={{ color: '#6a8f6a' }}>{info.tag} </span>}
                        {lineText(e)}
                        {info.loc && <span style={{ color: '#555' }}> · {info.loc}</span>}
                      </span>
                    </div>
                  )
                })}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  )
}
