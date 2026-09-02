import { useEffect, useRef, useState } from 'react'

import { useT } from '@/stores/i18n'
import { useNavigationRequestStore } from '@/stores/navigation-request'
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

const EMPTY_DRAFT: Draft = { scheme: 'http', host: '127.0.0.1', port: '8080', username: '', password: '' }

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
  const t = useT()
  const setTabProxy = useTabsStore((s) => s.setTabProxy)
  const requestNav = useNavigationRequestStore((s) => s.request)
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
      setError(t('proxy.errHost'))
      return
    }
    if (!Number.isInteger(port) || port <= 0 || port > 65535) {
      setError(t('proxy.errPort'))
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

    // Already an isolated proxy tab → edit its proxy in place.
    if (tab.partition) {
      setTabProxy(tab.id, config)
      try {
        await window.rev.proxy.set(tab.id, config)
      } catch (e) {
        console.error('[proxy] set failed', e)
        setError(t('proxy.errApply'))
        return
      }
      setOpen(false)
      onApplied()
      return
    }

    // Normal tab → open a NEW tab in its own in-memory partition (separate
    // cookie jar, incognito-style) routed through the proxy. The tab starts
    // on about:blank so no request leaves before the proxy is registered,
    // then navigates to the current page.
    const partition = `rever-proxy-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`
    const target = /^https?:\/\//i.test(tab.url) ? tab.url : 'https://www.google.com'
    const newId = useTabsStore.getState().addTab('about:blank', { proxy: config, partition })
    try {
      await window.rev.proxy.set(newId, config, partition)
    } catch (e) {
      console.error('[proxy] set failed', e)
      useTabsStore.getState().closeTab(newId)
      setError(t('proxy.errApply'))
      return
    }
    requestNav(target)
    setOpen(false)
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

  const label =
    on && proxy
      ? t('proxy.labelActive', { host: proxy.host, port: proxy.port })
      : t('proxy.label')

  return (
    <div style={{ position: 'relative', flexShrink: 0 }}>
      <button
        className="toolbar-btn"
        type="button"
        onClick={() => setOpen((v) => !v)}
        disabled={!tab}
        title={
          on && proxy
            ? t('proxy.editTitle', {
                scheme: proxy.scheme,
                host: proxy.host,
                port: proxy.port
              })
            : t('proxy.setTitle')
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
            className="proxy-popover"
            style={{
              position: 'absolute',
              top: 'calc(100% + 6px)',
              right: 0,
              zIndex: 41,
              width: 286,
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
            <div style={{ fontWeight: 600, color: 'var(--text)' }}>{t('proxy.tabProxy')}</div>

            <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)' }}>
              {t('proxy.scheme')}
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
              <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)', flex: 1, minWidth: 0 }}>
                {t('proxy.host')}
                <input
                  value={draft.host}
                  onChange={(e) => setDraft((d) => ({ ...d, host: e.target.value }))}
                  placeholder="127.0.0.1"
                  style={{ height: 26, padding: '0 6px', fontFamily: 'ui-monospace, monospace' }}
                />
              </label>
              <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)', flex: 1, minWidth: 0 }}>
                {t('proxy.port')}
                <input
                  value={draft.port}
                  onChange={(e) => setDraft((d) => ({ ...d, port: e.target.value.replace(/[^0-9]/g, '') }))}
                  placeholder="8080"
                  inputMode="numeric"
                  style={{ height: 26, padding: '0 6px', fontFamily: 'ui-monospace, monospace' }}
                />
              </label>
            </div>

            <div style={{ display: 'flex', gap: 6, alignItems: 'flex-end' }}>
              <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)', flex: 1, minWidth: 0 }}>
                <span style={{ whiteSpace: 'nowrap' }}>
                  {t('proxy.user')} <span style={{ opacity: 0.6 }}>{t('proxy.optional')}</span>
                </span>
                <input
                  value={draft.username}
                  onChange={(e) => setDraft((d) => ({ ...d, username: e.target.value }))}
                  style={{ height: 26, padding: '0 6px' }}
                />
              </label>
              <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)', flex: 1, minWidth: 0 }}>
                {t('proxy.password')}
                <input
                  type="password"
                  value={draft.password}
                  onChange={(e) => setDraft((d) => ({ ...d, password: e.target.value }))}
                  style={{ height: 26, padding: '0 6px' }}
                />
              </label>
            </div>

            {error && <div style={{ color: 'var(--status-error)' }}>{error}</div>}

            <div style={{ display: 'flex', gap: 6, justifyContent: 'flex-end', marginTop: 2 }}>
              {on && (
                <button type="button" onClick={disable} style={{ marginRight: 'auto' }}>
                  {t('proxy.disable')}
                </button>
              )}
              <button type="button" onClick={() => setOpen(false)}>
                {t('common.cancel')}
              </button>
              <button
                type="button"
                onClick={apply}
                style={{ background: 'var(--accent)', color: '#fff', borderColor: 'var(--accent)' }}
              >
                {tab?.partition ? t('proxy.apply') : t('proxy.openTab')}
              </button>
            </div>

            <div style={{ color: 'var(--text-dim)', opacity: 0.8, lineHeight: 1.4 }}>
              {tab?.partition ? t('proxy.noteEdit') : t('proxy.noteNew')}
            </div>
          </div>
        </>
      )}
    </div>
  )
}
