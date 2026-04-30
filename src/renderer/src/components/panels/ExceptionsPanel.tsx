import { useEffect, useRef, useState } from 'react'

interface RuntimeException {
  ts: number
  text: string
  exception?: unknown
  stackTrace?: unknown
}

interface ExItem extends RuntimeException {
  expanded: boolean
}

export function ExceptionsPanel() {
  const [items, setItems] = useState<ExItem[]>([])
  const fetchedRef = useRef(false)

  useEffect(() => {
    const poll = async () => {
      const next = await window.rev.console.exceptions()
      setItems((prev) => {
        const lastTsChanged = next[next.length - 1]?.ts !== prev[prev.length - 1]?.ts
        if (next.length === prev.length && !lastTsChanged) return prev
        return next.map((e, i) => ({ ...e, expanded: prev[i]?.expanded ?? false }))
      })
    }
    if (!fetchedRef.current) {
      fetchedRef.current = true
      void poll()
    }
    const id = setInterval(() => void poll(), 1000)
    return () => clearInterval(id)
  }, [])

  const toggle = (i: number) =>
    setItems((prev) => prev.map((item, idx) => idx === i ? { ...item, expanded: !item.expanded } : item))

  return (
    <div style={{ flex: 1, overflowY: 'auto', fontFamily: 'ui-monospace, monospace', fontSize: 11, scrollbarGutter: 'stable' }}>
      {items.map((e, i) => (
        <div
          key={i}
          style={{
            padding: '6px 10px',
            borderBottom: '1px solid rgba(255,255,255,0.05)',
            cursor: e.stackTrace ? 'pointer' : 'default'
          }}
          onClick={() => e.stackTrace && toggle(i)}
        >
          <div style={{ display: 'flex', gap: 8, alignItems: 'flex-start' }}>
            <span style={{ opacity: 0.4, flexShrink: 0, fontSize: 10 }}>
              {new Date(e.ts).toLocaleTimeString()}
            </span>
            <span style={{ color: '#ff6b6b', wordBreak: 'break-all', whiteSpace: 'pre-wrap' }}>
              {e.text}
            </span>
            {Boolean(e.stackTrace) && (
              <span style={{ opacity: 0.4, flexShrink: 0 }}>{e.expanded ? '▲' : '▼'}</span>
            )}
          </div>
          {e.expanded && Boolean(e.stackTrace) && (
            <pre
              style={{
                margin: '4px 0 0 0',
                padding: '6px 8px',
                background: 'rgba(255,107,107,0.06)',
                borderRadius: 4,
                fontSize: 10,
                color: '#ff9999',
                whiteSpace: 'pre-wrap',
                wordBreak: 'break-all',
                maxHeight: 160,
                overflowY: 'auto'
              }}
            >
              {JSON.stringify(e.stackTrace, null, 2)}
            </pre>
          )}
        </div>
      ))}
      {items.length === 0 && (
        <div style={{ padding: '12px 10px', opacity: 0.4, textAlign: 'center' }}>No exceptions</div>
      )}
    </div>
  )
}
