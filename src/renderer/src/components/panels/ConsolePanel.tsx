import { useEffect, useRef, useState } from 'react'

interface ConsoleEntry {
  ts: number
  type: string
  text: string
}

const TYPE_COLOR: Record<string, string> = {
  error: '#ff6b6b',
  warning: '#ffd93d',
  warn: '#ffd93d',
  info: '#74b9ff',
  log: 'var(--text)',
  debug: 'var(--text-dim)'
}

export function ConsolePanel() {
  const [entries, setEntries] = useState<ConsoleEntry[]>([])
  const lastTsRef = useRef<number>(0)
  const scrollRef = useRef<HTMLDivElement>(null)

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
    void poll()
    const id = setInterval(() => void poll(), 1000)
    return () => clearInterval(id)
  }, [])

  const handleClear = async () => {
    await window.rev.console.clear()
    setEntries([])
    lastTsRef.current = 0
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'flex-end',
          padding: '4px 10px',
          borderBottom: '1px solid rgba(255,255,255,0.06)',
          flexShrink: 0
        }}
      >
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
              borderBottom: '1px solid rgba(255,255,255,0.03)',
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
              {e.type}
            </span>
            <span style={{ wordBreak: 'break-all', whiteSpace: 'pre-wrap' }}>{e.text}</span>
          </div>
        ))}
        {entries.length === 0 && (
          <div style={{ padding: '12px 10px', opacity: 0.4, textAlign: 'center' }}>No console output</div>
        )}
      </div>
    </div>
  )
}
