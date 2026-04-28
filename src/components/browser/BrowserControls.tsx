import { invoke } from '@tauri-apps/api/core'
import { useState, type FormEvent } from 'react'

export function BrowserControls() {
  const [url, setUrl] = useState('https://www.google.com')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const run = async (action: () => Promise<unknown>) => {
    setBusy(true)
    setError(null)
    try {
      await action()
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  const onSubmit = (e: FormEvent) => {
    e.preventDefault()
    let target = url.trim()
    if (!target) return
    if (!/^https?:\/\//i.test(target)) {
      target = 'https://' + target
      setUrl(target)
    }
    void run(() => invoke('browser_navigate', { url: target }))
  }

  return (
    <div
      style={{
        display: 'flex',
        gap: 6,
        alignItems: 'center',
        padding: '8px 12px',
        borderBottom: '1px solid #333'
      }}
    >
      <button onClick={() => run(() => invoke('browser_back'))} disabled={busy} title="Back">
        ←
      </button>
      <button onClick={() => run(() => invoke('browser_forward'))} disabled={busy} title="Forward">
        →
      </button>
      <button onClick={() => run(() => invoke('browser_reload'))} disabled={busy} title="Reload">
        ↻
      </button>
      <form onSubmit={onSubmit} style={{ flex: 1, display: 'flex', gap: 6 }}>
        <input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://..."
          style={{ flex: 1, padding: '4px 8px', fontFamily: 'ui-monospace, monospace' }}
        />
        <button type="submit" disabled={busy}>
          Go
        </button>
      </form>
      {error && <span style={{ color: '#f55', fontSize: 11 }}>{error}</span>}
    </div>
  )
}
