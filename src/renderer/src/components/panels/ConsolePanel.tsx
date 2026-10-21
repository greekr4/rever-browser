import { useT } from '@/stores/i18n'

import { useEffect, useRef, useState, type KeyboardEvent } from 'react'

interface ConsoleEntry {
  ts: number
  type: string
  text: string
}

const TYPE_COLOR: Record<string, string> = {
  error: 'var(--status-error)',
  warning: 'var(--status-warn)',
  warn: 'var(--status-warn)',
  info: 'var(--status-info)',
  log: 'var(--text)',
  debug: 'var(--text-dim)',
  command: 'var(--text-2)',
  result: 'var(--text)'
}

// Chrome-style markers for lines typed into the command line and their output.
const TYPE_LABEL: Record<string, string> = { command: '›', result: '‹' }

export function ConsolePanel() {
  const t = useT()
  const [entries, setEntries] = useState<ConsoleEntry[]>([])
  const lastTsRef = useRef<number>(0)
  const scrollRef = useRef<HTMLDivElement>(null)
  const pollRef = useRef<() => Promise<void>>(async () => {})
  const [input, setInput] = useState('')
  const historyRef = useRef<string[]>([])
  const histIdxRef = useRef(-1)

  useEffect(() => {
    const poll = async () => {
      const next = await window.rev.console.list(lastTsRef.current)
      if (next.length > 0) {
        lastTsRef.current = next[next.length - 1].ts + 1
        setEntries((prev) => [...prev, ...next])
        requestAnimationFrame(() => {
          const el = scrollRef.current
          if (el) el.scrollTop = el.scrollHeight
        })
      }
    }
    pollRef.current = poll
    void poll()
    const id = setInterval(() => void poll(), 1000)
    return () => clearInterval(id)
  }, [])

  const handleClear = async () => {
    await window.rev.console.clear()
    setEntries([])
    lastTsRef.current = 0
  }

  const run = async () => {
    const expr = input.trim()
    if (!expr) return
    historyRef.current.push(expr)
    histIdxRef.current = -1
    setInput('')
    await window.rev.console.evaluate(expr)
    await pollRef.current()
  }

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      void run()
      return
    }
    // Up/Down walk the command history, but only from a single-line input.
    if ((e.key !== 'ArrowUp' && e.key !== 'ArrowDown') || input.includes('\n')) return
    const hist = historyRef.current
    if (hist.length === 0) return
    e.preventDefault()
    let i = histIdxRef.current
    if (e.key === 'ArrowUp') i = i === -1 ? hist.length - 1 : Math.max(0, i - 1)
    else i = i === -1 || i >= hist.length - 1 ? -1 : i + 1
    histIdxRef.current = i
    setInput(i === -1 ? '' : hist[i])
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <div className="panel-toolbar" style={{ justifyContent: 'flex-end' }}>
        <button
          onClick={() => void handleClear()}
          style={{ fontSize: 11, padding: '2px 8px' }}
        >
          Clear
        </button>
      </div>
      <div ref={scrollRef} style={{ flex: 1, overflowY: 'auto', fontFamily: 'ui-monospace, monospace', fontSize: 11, scrollbarGutter: 'stable' }}>
        {entries.map((e, i) => (
          <div
            key={i}
            style={{
              padding: '2px 10px',
              borderBottom: '1px solid var(--border)',
              color: TYPE_COLOR[e.type] ?? 'var(--text)',
              display: 'flex',
              gap: 8,
              alignItems: 'flex-start'
            }}
          >
            <span style={{ opacity: 0.4, flexShrink: 0, fontSize: 10 }}>
              {new Date(e.ts).toLocaleTimeString()}
            </span>
            <span style={{ opacity: 0.6, flexShrink: 0, fontSize: 10, minWidth: 36 }}>
              {TYPE_LABEL[e.type] ?? e.type}
            </span>
            <span style={{ wordBreak: 'break-all', whiteSpace: 'pre-wrap' }}>{e.text}</span>
          </div>
        ))}
        {entries.length === 0 && (
          <div className="panel-empty">{t('panel.noConsole')}</div>
        )}
      </div>
      <textarea
        value={input}
        onChange={(e) => setInput(e.target.value)}
        onKeyDown={onKeyDown}
        placeholder={t('panel.consoleInput')}
        aria-label={t('panel.consoleInput')}
        spellCheck={false}
        rows={Math.min(6, input.split('\n').length)}
        style={{
          resize: 'none',
          border: 'none',
          borderTop: '1px solid var(--border)',
          padding: '4px 10px',
          fontFamily: 'ui-monospace, monospace',
          fontSize: 11,
          background: 'var(--bg)',
          color: 'var(--text)',
          outline: 'none'
        }}
      />
    </div>
  )
}
