import '@xterm/xterm/css/xterm.css'

import { useEffect, useRef, useState } from 'react'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'

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

function Onboarding({ onStart }: { onStart: () => void }) {
  return (
    <div
      style={{
        height: '100%',
        display: 'flex',
        flexDirection: 'column',
        gap: 14,
        padding: 20,
        overflow: 'auto',
        color: 'var(--text-2)',
        fontSize: 13,
        lineHeight: 1.5
      }}
    >
      <div style={{ fontSize: 15, fontWeight: 600, color: 'var(--text)' }}>Terminal (CLI) mode</div>
      <p style={{ margin: 0 }}>
        Runs the local <strong>Claude Code CLI</strong> in a real terminal, with Rever&apos;s browser
        &amp; traffic tools wired in automatically — the full CLI instead of the structured ACP chat.
      </p>

      <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
        <Step n={1} title="Requires the claude CLI">
          Install and log in once (uses your own Claude subscription):{' '}
          <code style={codeStyle}>npm i -g @anthropic-ai/claude-code</code>, then{' '}
          <code style={codeStyle}>claude</code>.
        </Step>
        <Step n={2} title="MCP connects automatically">
          Rever&apos;s ~140 tools are injected via <code style={codeStyle}>--mcp-config</code>. In the
          terminal, run <code style={codeStyle}>/mcp</code> to confirm the <strong>rever</strong>{' '}
          server is connected.
        </Step>
        <Step n={3} title="Drive the browser from the CLI">
          Ask it to navigate, capture traffic, or reverse an API — it uses the same live tab as the
          rest of Rever.
        </Step>
      </div>

      <p style={{ margin: 0, fontSize: 11, color: 'var(--text-dim)' }}>
        Switching back to <strong>Chat (ACP)</strong> ends the terminal session. macOS/Linux only.
      </p>

      <button
        type="button"
        onClick={onStart}
        style={{
          alignSelf: 'flex-start',
          padding: '8px 16px',
          fontSize: 13,
          background: 'var(--accent)',
          border: '1px solid var(--accent)',
          borderRadius: 6,
          color: '#fff',
          cursor: 'pointer'
        }}
      >
        Start Claude Code
      </button>
    </div>
  )
}

function Step({ n, title, children }: { n: number; title: string; children: React.ReactNode }) {
  return (
    <div style={{ display: 'flex', gap: 10 }}>
      <div
        style={{
          flexShrink: 0,
          width: 20,
          height: 20,
          borderRadius: '50%',
          background: 'var(--accent-soft)',
          color: 'var(--accent)',
          fontSize: 11,
          fontWeight: 700,
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center'
        }}
      >
        {n}
      </div>
      <div>
        <div style={{ color: 'var(--text)', fontWeight: 600, marginBottom: 2 }}>{title}</div>
        <div>{children}</div>
      </div>
    </div>
  )
}

const codeStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  fontSize: 11,
  background: 'var(--bg)',
  border: '1px solid var(--border-2)',
  borderRadius: 3,
  padding: '1px 4px'
}

function TerminalView({ onShowGuide }: { onShowGuide: () => void }) {
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
          title="Show CLI mode guide"
          style={{
            background: 'transparent',
            border: 'none',
            color: 'var(--text-dim)',
            cursor: 'pointer',
            fontSize: 11,
            padding: '2px 6px'
          }}
        >
          ⓘ Guide
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
          <span>Agent exited (code {exited}).</span>
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
            Restart
          </button>
        </div>
      )}
    </div>
  )
}
