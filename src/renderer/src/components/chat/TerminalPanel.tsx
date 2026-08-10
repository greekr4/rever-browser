import '@xterm/xterm/css/xterm.css'

import { useEffect, useRef, useState } from 'react'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'

import { useT } from '@/stores/i18n'

// A local CLI agent (Claude Code) running in a real PTY, wired to rever's MCP.
// First switch shows an onboarding card; after that it goes straight to the
// terminal (remembered in localStorage).

const ONBOARDED_KEY = 'rev:cli-onboarded'

export function TerminalPanel() {
  const [started, setStarted] = useState(() => localStorage.getItem(ONBOARDED_KEY) === '1')

  const start = (): void => {
    localStorage.setItem(ONBOARDED_KEY, '1')
    setStarted(true)
  }

  return started ? (
    <TerminalView onShowGuide={() => setStarted(false)} />
  ) : (
    <Onboarding onStart={start} />
  )
}

const INSTALL_CMD = 'npx skills add greekr4/rever-browser-skill --global --agent claude-code'

function Onboarding({ onStart }: { onStart: () => void }) {
  const t = useT()
  const [installed, setInstalled] = useState<boolean | null>(null)
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    void window.rev.skill.status().then((s) => setInstalled(s.installed))
  }, [])

  const copy = (): void => {
    void navigator.clipboard.writeText(INSTALL_CMD).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }

  return (
    <div
      style={{
        height: '100%',
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        justifyContent: 'center',
        textAlign: 'center',
        gap: 18,
        padding: '28px 24px',
        overflow: 'auto'
      }}
    >
      <div style={{ maxWidth: 300, display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 8 }}>
        <div style={{ fontSize: 26, letterSpacing: 4, color: 'var(--accent)', fontFamily: 'ui-monospace, monospace' }}>
          &gt;_
        </div>
        <div style={{ fontSize: 17, fontWeight: 700, color: 'var(--text)' }}>{t('cli.title')}</div>
        <p style={{ margin: 0, fontSize: 13, color: 'var(--text-dim)', lineHeight: 1.5 }}>
          {t('cli.desc')}
        </p>
      </div>

      {/* Primary path: jump straight into the terminal. */}
      <button
        type="button"
        onClick={onStart}
        style={{
          width: 240,
          maxWidth: '100%',
          padding: '11px 16px',
          fontSize: 14,
          fontWeight: 600,
          background: 'var(--accent)',
          border: '1px solid var(--accent)',
          borderRadius: 8,
          color: '#fff',
          cursor: 'pointer'
        }}
      >
        {t('cli.start')}
      </button>

      {/* Secondary: the /rever skill for OTHER Claude Code sessions. Show the
          command and let the user run it in their own terminal. */}
      <div style={{ width: 300, maxWidth: '100%', paddingTop: 8, borderTop: '1px solid var(--border)', display: 'flex', flexDirection: 'column', gap: 8 }}>
        <div style={{ fontSize: 12, color: 'var(--text-dim)', lineHeight: 1.5 }}>
          {t('cli.skillPrompt')}
        </div>
        <div
          onClick={copy}
          title="Click to copy"
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            padding: '8px 10px',
            background: 'var(--bg)',
            border: '1px solid var(--border-2)',
            borderRadius: 8,
            cursor: 'pointer',
            textAlign: 'left'
          }}
        >
          <code style={{ flex: 1, fontFamily: 'ui-monospace, monospace', fontSize: 11, color: 'var(--text-2)', wordBreak: 'break-all', lineHeight: 1.4 }}>
            {INSTALL_CMD}
          </code>
          <span style={{ flexShrink: 0, fontSize: 11, color: copied ? 'var(--status-ok)' : 'var(--text-dim)' }}>
            {copied ? t('cli.copied') : t('cli.copy')}
          </span>
        </div>
        {installed && (
          <div style={{ fontSize: 11, color: 'var(--status-ok)' }}>{t('cli.installed')}</div>
        )}
      </div>

      <p style={{ margin: 0, maxWidth: 260, fontSize: 11, color: 'var(--text-dim)', lineHeight: 1.5 }}>
        {t('cli.requires')}
      </p>
    </div>
  )
}


function TerminalView({ onShowGuide }: { onShowGuide: () => void }) {
  const t = useT()
  const hostRef = useRef<HTMLDivElement>(null)
  const [exited, setExited] = useState<number | null>(null)
  const [restartKey, setRestartKey] = useState(0)

  useEffect(() => {
    const host = hostRef.current
    if (!host) return
    setExited(null)

    const term = new Terminal({
      cursorBlink: true,
      fontSize: 12,
      fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
      theme: { background: '#0b0d12', foreground: '#d7dae0' }
    })
    const fit = new FitAddon()
    term.loadAddon(fit)
    term.open(host)
    fit.fit()

    let termId: string | null = null
    let offData: (() => void) | null = null
    let offExit: (() => void) | null = null
    let disposed = false

    void window.rev.terminal
      .spawn({ cols: term.cols, rows: term.rows, agent: 'claude' })
      .then((id) => {
        if (disposed) {
          window.rev.terminal.kill(id)
          return
        }
        termId = id
        offData = window.rev.terminal.onData(id, (data) => term.write(data))
        offExit = window.rev.terminal.onExit(id, (code) => setExited(code))
        term.onData((d) => window.rev.terminal.write(id, d))
      })

    const doFit = (): void => {
      try {
        fit.fit()
        if (termId) window.rev.terminal.resize(termId, term.cols, term.rows)
      } catch {
        // Terminal disposed mid-resize — ignore.
      }
    }
    const ro = new ResizeObserver(doFit)
    ro.observe(host)

    return () => {
      disposed = true
      ro.disconnect()
      offData?.()
      offExit?.()
      if (termId) window.rev.terminal.kill(termId)
      term.dispose()
    }
  }, [restartKey])

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', minHeight: 0 }}>
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'flex-end',
          padding: '2px 6px',
          borderBottom: '1px solid var(--border)',
          flexShrink: 0
        }}
      >
        <button
          type="button"
          onClick={onShowGuide}
          title={t('cli.guideTitle')}
          style={{
            background: 'transparent',
            border: 'none',
            color: 'var(--text-dim)',
            cursor: 'pointer',
            fontSize: 11,
            padding: '2px 6px'
          }}
        >
          ⓘ {t('cli.guide')}
        </button>
      </div>
      <div ref={hostRef} style={{ flex: 1, minHeight: 0, padding: 6, background: '#0b0d12' }} />
      {exited !== null && (
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            padding: '6px 10px',
            fontSize: 11,
            color: 'var(--text-dim)',
            borderTop: '1px solid var(--border)'
          }}
        >
          <span>{t('cli.exited', { code: exited })}</span>
          <button
            type="button"
            onClick={() => setRestartKey((k) => k + 1)}
            style={{
              padding: '3px 10px',
              fontSize: 11,
              background: 'var(--accent)',
              border: '1px solid var(--accent)',
              borderRadius: 4,
              color: '#fff',
              cursor: 'pointer'
            }}
          >
            {t('cli.restart')}
          </button>
        </div>
      )}
    </div>
  )
}
