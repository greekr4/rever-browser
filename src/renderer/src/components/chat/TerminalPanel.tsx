import '@xterm/xterm/css/xterm.css'

import { useEffect, useRef, useState } from 'react'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'

// A local CLI agent (Claude Code) running in a real PTY, wired to rever's MCP.
// The xterm instance lives for the panel's lifetime; the PTY is spawned in main.

export function TerminalPanel() {
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
    // restartKey forces a fresh PTY + xterm when the user restarts.
  }, [restartKey])

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', minHeight: 0 }}>
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
