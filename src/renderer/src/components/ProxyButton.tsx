import { useEffect, useRef, useState } from 'react'

import type { ProxyConfig, Tab } from '@/stores/tabs'
import { useTabsStore } from '@/stores/tabs'

interface Props {
  tab: Tab | undefined
  // Called after a proxy change is applied so the caller can reload the tab
  // (Electron applies the proxy to new requests; a reload makes it take full
  // effect for the visible page).
  onApplied: () => void
}

interface Draft {
  scheme: ProxyConfig['scheme']
  host: string
  port: string
  username: string
  password: string
}

const EMPTY_DRAFT: Draft = { scheme: 'http', host: '', port: '8080', username: '', password: '' }

function draftFrom(proxy: ProxyConfig | undefined): Draft {
  if (!proxy) return { ...EMPTY_DRAFT }
  return {
    scheme: proxy.scheme,
    host: proxy.host,
    port: String(proxy.port),
    username: proxy.username ?? '',
    password: proxy.password ?? ''
  }
}

export function ProxyButton({ tab, onApplied }: Props): React.ReactElement {
  const setTabProxy = useTabsStore((s) => s.setTabProxy)
  const [open, setOpen] = useState(false)
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT)
  const [error, setError] = useState<string | null>(null)
  const firstFieldRef = useRef<HTMLSelectElement>(null)

  const proxy = tab?.proxy
  const on = !!proxy?.enabled

  // Load the active tab's config each time the popover opens.
  useEffect(() => {
    if (open) {
      setDraft(draftFrom(tab?.proxy))
      setError(null)
      firstFieldRef.current?.focus()
    }
  }, [open, tab?.id, tab?.proxy])

  // ESC closes.
  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') setOpen(false)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [open])

  const apply = async (): Promise<void> => {
    if (!tab) return
    const host = draft.host.trim()
    const port = Number(draft.port)
    if (!host) {
      setError('Host is required')
      return
    }
    if (!Number.isInteger(port) || port <= 0 || port > 65535) {
      setError('Port must be 1–65535')
      return
    }
    const config: ProxyConfig = {
      enabled: true,
      scheme: draft.scheme,
      host,
      port,
      username: draft.username.trim() || undefined,
      password: draft.password || undefined
    }
    setTabProxy(tab.id, config)
    try {
      await window.rev.proxy.set(tab.id, config)
    } catch (e) {
      console.error('[proxy] set failed', e)
      setError('Failed to apply proxy')
      return
    }
    setOpen(false)
    onApplied()
  }

  const disable = async (): Promise<void> => {
    if (!tab) return
    setTabProxy(tab.id, undefined)
    try {
      await window.rev.proxy.set(tab.id, null)
    } catch (e) {
      console.error('[proxy] clear failed', e)
    }
    setOpen(false)
    onApplied()
  }

  const label = on && proxy ? `Proxy: ${proxy.host}:${proxy.port}` : 'Proxy'

  return (
    <div style={{ position: 'relative', flexShrink: 0 }}>
      <button
        className="toolbar-btn"
        type="button"
        onClick={() => setOpen((v) => !v)}
        disabled={!tab}
        title={
          on && proxy
            ? `Tab proxy: ${proxy.scheme}://${proxy.host}:${proxy.port} — click to edit`
            : 'Set a proxy for this tab'
        }
        style={{
          maxWidth: 180,
          overflow: 'hidden',
          textOverflow: 'ellipsis',
          background: on ? 'var(--accent-soft)' : undefined,
          borderColor: on ? 'var(--accent-border)' : undefined,
          color: on ? 'var(--accent)' : undefined
        }}
      >
        {label}
      </button>

      {open && (
        <>
          {/* click-outside backdrop */}
          <div
            onClick={() => setOpen(false)}
            style={{ position: 'fixed', inset: 0, zIndex: 40 }}
          />
          <div
            style={{
              position: 'absolute',
              top: 'calc(100% + 6px)',
              right: 0,
              zIndex: 41,
              width: 260,
              padding: 12,
              display: 'flex',
              flexDirection: 'column',
              gap: 8,
              background: 'var(--surface)',
              border: '1px solid var(--border-2)',
              borderRadius: 8,
              boxShadow: '0 8px 24px var(--shadow)',
              fontSize: 12
            }}
          >
            <div style={{ fontWeight: 600, color: 'var(--text)' }}>Tab proxy</div>

            <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)' }}>
              Scheme
              <select
                ref={firstFieldRef}
                value={draft.scheme}
                onChange={(e) => setDraft((d) => ({ ...d, scheme: e.target.value as Draft['scheme'] }))}
                style={{ height: 26, padding: '0 6px' }}
              >
                <option value="http">http</option>
                <option value="https">https</option>
                <option value="socks5">socks5</option>
              </select>
            </label>

            <div style={{ display: 'flex', gap: 6 }}>
              <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)', flex: 1 }}>
                Host
                <input
                  value={draft.host}
                  onChange={(e) => setDraft((d) => ({ ...d, host: e.target.value }))}
                  placeholder="127.0.0.1"
                  style={{ height: 26, padding: '0 6px', fontFamily: 'ui-monospace, monospace' }}
                />
              </label>
              <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)', width: 74 }}>
                Port
                <input
                  value={draft.port}
                  onChange={(e) => setDraft((d) => ({ ...d, port: e.target.value.replace(/[^0-9]/g, '') }))}
                  placeholder="8080"
                  inputMode="numeric"
                  style={{ height: 26, padding: '0 6px', fontFamily: 'ui-monospace, monospace' }}
                />
              </label>
            </div>

            <div style={{ display: 'flex', gap: 6 }}>
              <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)', flex: 1 }}>
                User <span style={{ opacity: 0.6 }}>(optional)</span>
                <input
                  value={draft.username}
                  onChange={(e) => setDraft((d) => ({ ...d, username: e.target.value }))}
                  style={{ height: 26, padding: '0 6px' }}
                />
              </label>
              <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)', flex: 1 }}>
                Password
                <input
                  type="password"
                  value={draft.password}
                  onChange={(e) => setDraft((d) => ({ ...d, password: e.target.value }))}
                  style={{ height: 26, padding: '0 6px' }}
                />
              </label>
            </div>

            {error && <div style={{ color: '#e06c6c' }}>{error}</div>}

            <div style={{ display: 'flex', gap: 6, justifyContent: 'flex-end', marginTop: 2 }}>
              {on && (
                <button type="button" onClick={disable} style={{ marginRight: 'auto' }}>
                  Disable
                </button>
              )}
              <button type="button" onClick={() => setOpen(false)}>
                Cancel
              </button>
              <button
                type="button"
                onClick={apply}
                style={{ background: 'var(--accent)', color: '#fff', borderColor: 'var(--accent)' }}
              >
                Apply
              </button>
            </div>

            <div style={{ color: 'var(--text-dim)', opacity: 0.8, lineHeight: 1.4 }}>
              Applies to this tab only; reloads the page. Cookies/storage are
              isolated per tab.
            </div>
          </div>
        </>
      )}
    </div>
  )
}
