import { useEffect, useLayoutEffect, useRef, useState } from 'react'

import { useT } from '@/stores/i18n'
import { useProfilesStore } from '@/stores/profiles'
import type { Tab } from '@/stores/tabs'
import { useTabsStore } from '@/stores/tabs'

export function TabBar() {
  const tr = useT()
  const tabs = useTabsStore((s) => s.tabs)
  const activeId = useTabsStore((s) => s.activeId)
  const selectTab = useTabsStore((s) => s.selectTab)
  const closeTab = useTabsStore((s) => s.closeTab)
  const addTab = useTabsStore((s) => s.addTab)

  const profiles = useProfilesStore((s) => s.profiles)

  // Chrome-style overflow: tabs that don't fit are hidden from the strip and
  // listed under a "⋯" button instead of a horizontal scrollbar.
  const stripRef = useRef<HTMLDivElement>(null)
  const tabEls = useRef(new Map<string, HTMLDivElement>())
  const widths = useRef(new Map<string, number>())
  const [overflowIds, setOverflowIds] = useState<string[]>([])
  const [measureTick, setMeasureTick] = useState(0)
  const [tabMenuOpen, setTabMenuOpen] = useState(false)
  const [tabMenuPos, setTabMenuPos] = useState<{ top: number; right: number }>({ top: 0, right: 0 })
  const ofBtnRef = useRef<HTMLButtonElement>(null)

  // Recompute which tabs fit on resize of the strip (which also shrinks when the
  // "⋯" button appears, so the observer settles the layout in a frame or two).
  useEffect(() => {
    const strip = stripRef.current
    if (!strip) return
    const ro = new ResizeObserver(() => setMeasureTick((n) => n + 1))
    ro.observe(strip)
    return () => ro.disconnect()
  }, [])

  useLayoutEffect(() => {
    const strip = stripRef.current
    if (!strip) return
    // Record widths of currently-mounted tabs; overflow tabs are unmounted from
    // the strip, so their last-measured width is kept (prune only closed tabs).
    for (const [id, el] of tabEls.current) widths.current.set(id, el.offsetWidth)
    const live = new Set(tabs.map((t) => t.id))
    for (const id of [...widths.current.keys()]) if (!live.has(id)) widths.current.delete(id)

    // The + button lives inside the strip (Chrome puts it right after the last tab).
    const avail = strip.clientWidth - 36
    const next: string[] = []
    // The active tab is always kept visible — reserve its width first.
    let used = activeId ? widths.current.get(activeId) ?? 140 : 0
    for (const t of tabs) {
      if (t.id === activeId) continue
      const w = widths.current.get(t.id) ?? 140
      if (used + w <= avail) used += w
      else next.push(t.id)
    }
    setOverflowIds((prev) =>
      prev.length === next.length && prev.every((v, i) => v === next[i]) ? prev : next
    )
  }, [tabs, activeId, measureTick])

  const toggleTabMenu = (): void => {
    setTabMenuOpen((v) => {
      if (!v && ofBtnRef.current) {
        const r = ofBtnRef.current.getBoundingClientRect()
        setTabMenuPos({ top: r.bottom + 4, right: window.innerWidth - r.right })
      }
      return !v
    })
  }

  const profileName = (id?: string): string | null => {
    if (!id || id === 'default') return null
    return profiles.find((p) => p.id === id)?.name ?? 'Profile'
  }

  const overflowSet = new Set(overflowIds)
  const overflowTabs = tabs.filter((t) => overflowSet.has(t.id))

  const renderTab = (t: Tab): React.ReactElement => {
    const isActive = t.id === activeId
    // Ephemeral partition (proxy / incognito) — cookies vanish with the app.
    // Persistent profile tabs (persist:…) render like normal tabs.
    const ephemeral = !!t.partition && !t.partition.startsWith('persist:')
    const label = profileName(t.profileId)
    return (
      <div
        key={t.id}
        ref={(el) => {
          if (el) tabEls.current.set(t.id, el)
          else tabEls.current.delete(t.id)
        }}
        onClick={() => selectTab(t.id)}
        onAuxClick={(e) => {
          if (e.button === 1) closeTab(t.id) // middle-click close
        }}
        className={isActive ? undefined : 'chrome-tab'}
        style={
          {
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            height: 34,
            padding: '0 8px 0 12px',
            boxSizing: 'border-box',
            minWidth: 120,
            maxWidth: 240,
            flexShrink: 0,
            cursor: 'pointer',
            // Chrome: the active tab is the toolbar's colour and flows into it.
            borderRadius: '8px 8px 0 0',
            background: ephemeral
              ? 'var(--proxy-tab-bg)'
              : isActive
                ? 'var(--chrome-toolbar)'
                : undefined,
            fontSize: 12,
            color: ephemeral
              ? 'var(--proxy-tab-text)'
              : isActive
                ? 'var(--text)'
                : 'var(--text-2)',
            opacity: ephemeral && !isActive ? 0.75 : 1,
            userSelect: 'none',
            WebkitAppRegion: 'no-drag'
          } as React.CSSProperties
        }
      >
        {ephemeral && (
          <span
            style={{
              width: 6,
              height: 6,
              borderRadius: 3,
              background: 'var(--proxy-tab-dot)',
              flexShrink: 0
            }}
            title={
              t.proxy?.enabled
                ? `Isolated proxy tab — ${t.proxy.host}:${t.proxy.port}, own cookies`
                : 'Isolated tab — own cookies (proxy disabled)'
            }
          />
        )}
        {label && (
          <span
            style={{
              flexShrink: 0,
              maxWidth: 70,
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
              fontSize: 10,
              padding: '1px 5px',
              borderRadius: 3,
              background: 'var(--border-2)',
              color: 'var(--text-dim)'
            }}
            title={`Profile: ${label}`}
          >
            {label}
          </span>
        )}
        <span
          style={{
            flex: 1,
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap'
          }}
          title={t.url}
        >
          {t.title || t.url}
        </span>
        {tabs.length > 1 && (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation()
              closeTab(t.id)
            }}
            className="chrome-tab-close"
            title={tr('tab.close')}
          >
            ×
          </button>
        )}
      </div>
    )
  }

  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'flex-end',
        gap: 2,
        flex: 1,
        minWidth: 0,
        height: '100%',
        overflow: 'hidden'
      }}
    >
      {/* Tab strip — clips overflow (no scrollbar); extra tabs go to the ⋯ menu */}
      <div
        ref={stripRef}
        style={{
          display: 'flex',
          alignItems: 'flex-end',
          gap: 2,
          flex: '1 1 auto',
          minWidth: 0,
          height: '100%',
          overflow: 'hidden'
        }}
      >
        {tabs.filter((t) => !overflowSet.has(t.id)).map(renderTab)}
        <button
          type="button"
          onClick={() => addTab('https://www.google.com')}
          className="chrome-icon-btn small"
          style={{ marginLeft: 4, marginBottom: 3 }}
          title={tr('tab.new')}
          aria-label={tr('tab.new')}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" aria-hidden="true">
            <path d="M12 5v14M5 12h14" />
          </svg>
        </button>
      </div>


      {overflowTabs.length > 0 && (
        <div style={{ flexShrink: 0 }}>
          <button
            ref={ofBtnRef}
            type="button"
            onClick={toggleTabMenu}
            className="chrome-chip"
            style={{ marginBottom: 3 }}
            title={tr('tab.overflow')}
          >
            ⋯ {overflowTabs.length}
          </button>

          {tabMenuOpen && (
            <>
              <div
                onClick={() => setTabMenuOpen(false)}
                style={{ position: 'fixed', inset: 0, zIndex: 40, WebkitAppRegion: 'no-drag' } as React.CSSProperties}
              />
              <div
                style={
                  {
                    position: 'fixed',
                    top: tabMenuPos.top,
                    right: tabMenuPos.right,
                    minWidth: 220,
                    maxWidth: 320,
                    maxHeight: '70vh',
                    overflowY: 'auto',
                    background: 'var(--bg)',
                    border: '1px solid var(--border-2)',
                    borderRadius: 6,
                    boxShadow: '0 4px 16px rgba(0,0,0,0.3)',
                    padding: 4,
                    zIndex: 41,
                    WebkitAppRegion: 'no-drag'
                  } as React.CSSProperties
                }
              >
                {overflowTabs.map((t) => (
                  <div key={t.id} style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
                    <button
                      type="button"
                      onClick={() => {
                        setTabMenuOpen(false)
                        selectTab(t.id)
                      }}
                      style={{
                        flex: 1,
                        minWidth: 0,
                        textAlign: 'left',
                        padding: '6px 8px',
                        background: 'transparent',
                        border: 'none',
                        borderRadius: 4,
                        color: 'var(--text)',
                        cursor: 'pointer',
                        fontSize: 12,
                        overflow: 'hidden',
                        textOverflow: 'ellipsis',
                        whiteSpace: 'nowrap'
                      }}
                      title={t.url}
                    >
                      {t.title || t.url}
                    </button>
                    {tabs.length > 1 && (
                      <button
                        type="button"
                        onClick={() => closeTab(t.id)}
                        style={{
                          width: 20,
                          height: 20,
                          background: 'transparent',
                          border: 'none',
                          color: 'var(--text-dim)',
                          cursor: 'pointer',
                          fontSize: 13,
                          flexShrink: 0
                        }}
                        title={tr('tab.close')}
                      >
                        ×
                      </button>
                    )}
                  </div>
                ))}
              </div>
            </>
          )}
        </div>
      )}
    </div>
  )
}
