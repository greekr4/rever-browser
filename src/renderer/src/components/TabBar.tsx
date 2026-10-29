import { useEffect, useLayoutEffect, useRef, useState } from 'react'

import type { BrowserInfo, BrowserProfileInfo } from '../../../preload'
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
  const loadProfiles = useProfilesStore((s) => s.load)
  const createProfile = useProfilesStore((s) => s.create)
  const removeProfile = useProfilesStore((s) => s.remove)
  const openProfileTab = useProfilesStore((s) => s.openTab)
  const importFromBrowser = useProfilesStore((s) => s.importFromBrowser)

  const [menuOpen, setMenuOpen] = useState(false)
  const [menuPos, setMenuPos] = useState<{ top: number; right: number }>({ top: 0, right: 0 })
  const [newName, setNewName] = useState('')
  const [browsers, setBrowsers] = useState<BrowserInfo[]>([])
  const [expandedBrowser, setExpandedBrowser] = useState<string | null>(null)
  const [importMsg, setImportMsg] = useState<string | null>(null)
  const btnRef = useRef<HTMLButtonElement>(null)

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

  useEffect(() => {
    void loadProfiles()
    void window.rev.storage.browsers().then(setBrowsers)
  }, [loadProfiles])

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

    const avail = strip.clientWidth
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

  const toggleMenu = (): void => {
    setMenuOpen((v) => {
      if (!v && btnRef.current) {
        const r = btnRef.current.getBoundingClientRect()
        setMenuPos({ top: r.bottom + 4, right: window.innerWidth - r.right })
      }
      return !v
    })
  }

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

  const create = async (kind: 'persistent' | 'incognito'): Promise<void> => {
    const profile = await createProfile(newName, kind)
    setNewName('')
    setMenuOpen(false)
    await openProfileTab(profile)
  }

  const importBrowser = async (
    browser: BrowserInfo,
    profile: BrowserProfileInfo
  ): Promise<void> => {
    setImportMsg(`Importing ${profile.name}…`)
    try {
      const r = await importFromBrowser(browser, profile)
      setImportMsg(
        r.ok
          ? `✓ ${profile.name}: imported ${r.imported}/${r.total} cookies`
          : `⚠ ${r.error ?? 'import failed'}`
      )
    } catch (e) {
      setImportMsg(`⚠ ${e instanceof Error ? e.message : String(e)}`)
    }
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
        style={
          {
            display: 'flex',
            alignItems: 'center',
            gap: 6,
            padding: '6px 8px 6px 10px',
            minWidth: 120,
            maxWidth: 220,
            flexShrink: 0,
            cursor: 'pointer',
            borderRadius: '6px 6px 0 0',
            background: ephemeral
              ? 'var(--proxy-tab-bg)'
              : isActive
                ? 'var(--bg)'
                : 'transparent',
            borderTop: `1px solid ${isActive ? (ephemeral ? 'var(--proxy-tab-border)' : 'var(--border-2)') : 'transparent'}`,
            borderLeft: `1px solid ${isActive ? (ephemeral ? 'var(--proxy-tab-border)' : 'var(--border-2)') : 'transparent'}`,
            borderRight: `1px solid ${isActive ? (ephemeral ? 'var(--proxy-tab-border)' : 'var(--border-2)') : 'transparent'}`,
            fontSize: 12,
            color: ephemeral
              ? 'var(--proxy-tab-text)'
              : isActive
                ? 'var(--text)'
                : 'var(--text-dim)',
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
            style={{
              width: 16,
              height: 16,
              padding: 0,
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              background: 'transparent',
              border: 'none',
              color: 'var(--text-dim)',
              cursor: 'pointer',
              fontSize: 14,
              lineHeight: 1
            }}
            title={tr('tab.close')}
          >
            ×
          </button>
        )}
      </div>
    )
  }

  const smallBtn = {
    padding: '4px 10px',
    marginBottom: 2,
    background: 'transparent',
    border: '1px solid var(--border-2)',
    borderRadius: 4,
    color: 'var(--text-2)',
    cursor: 'pointer',
    fontSize: 14,
    lineHeight: 1,
    flexShrink: 0,
    WebkitAppRegion: 'no-drag'
  } as React.CSSProperties

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
      </div>

      <button
        type="button"
        onClick={() => addTab('https://www.google.com')}
        style={{ ...smallBtn, marginLeft: 4 }}
        title={tr('tab.new')}
      >
        +
      </button>

      {overflowTabs.length > 0 && (
        <div style={{ marginBottom: 2, flexShrink: 0 }}>
          <button
            ref={ofBtnRef}
            type="button"
            onClick={toggleTabMenu}
            style={smallBtn}
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

      <div style={{ marginBottom: 2, flexShrink: 0 }}>
        <button ref={btnRef} type="button" onClick={toggleMenu} style={smallBtn} title={tr('tab.newInProfile')}>
          👤
        </button>

        {menuOpen && (
          <>
            {/* click-outside backdrop */}
            <div
              onClick={() => setMenuOpen(false)}
              style={{ position: 'fixed', inset: 0, zIndex: 40, WebkitAppRegion: 'no-drag' } as React.CSSProperties}
            />
          <div
            style={
              {
                position: 'fixed',
                top: menuPos.top,
                right: menuPos.right,
                minWidth: 200,
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
            <div
              style={{
                fontSize: 10,
                color: 'var(--text-dim)',
                padding: '4px 8px',
                textTransform: 'uppercase',
                letterSpacing: 0.5
              }}
            >
              {tr('profile.openIn')}
            </div>
            {profiles.map((p) => (
              <div
                key={p.id}
                style={{ display: 'flex', alignItems: 'center', gap: 4 }}
              >
                <button
                  type="button"
                  onClick={() => {
                    setMenuOpen(false)
                    void openProfileTab(p)
                  }}
                  style={{
                    flex: 1,
                    textAlign: 'left',
                    padding: '6px 8px',
                    background: 'transparent',
                    border: 'none',
                    borderRadius: 4,
                    color: 'var(--text)',
                    cursor: 'pointer',
                    fontSize: 12
                  }}
                >
                  {p.name}
                  {p.source && (
                    <span style={{ color: 'var(--text-dim)', marginLeft: 6, fontSize: 10 }}>
                      {p.source}
                    </span>
                  )}
                  {p.kind === 'incognito' && (
                    <span style={{ color: 'var(--text-dim)', marginLeft: 6, fontSize: 10 }}>
                      incognito
                    </span>
                  )}
                </button>
                {p.id !== 'default' && (
                  <button
                    type="button"
                    onClick={() => void removeProfile(p.id)}
                    style={{
                      width: 20,
                      height: 20,
                      background: 'transparent',
                      border: 'none',
                      color: 'var(--text-dim)',
                      cursor: 'pointer',
                      fontSize: 13
                    }}
                    title={tr('profile.delete')}
                  >
                    ×
                  </button>
                )}
              </div>
            ))}

            <div style={{ height: 1, background: 'var(--border-2)', margin: '4px 0' }} />

            <input
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              placeholder={tr('profile.newName')}
              style={{
                width: '100%',
                boxSizing: 'border-box',
                padding: '6px 8px',
                background: 'var(--bg-2, transparent)',
                border: '1px solid var(--border-2)',
                borderRadius: 4,
                color: 'var(--text)',
                fontSize: 12,
                marginBottom: 4
              }}
            />
            <div style={{ display: 'flex', gap: 4 }}>
              <button
                type="button"
                onClick={() => void create('persistent')}
                style={{
                  flex: 1,
                  padding: '6px 8px',
                  background: 'transparent',
                  border: '1px solid var(--border-2)',
                  borderRadius: 4,
                  color: 'var(--text-2)',
                  cursor: 'pointer',
                  fontSize: 11
                }}
                title={tr('profile.persistentHint')}
              >
                {tr('profile.persistent')}
              </button>
              <button
                type="button"
                onClick={() => void create('incognito')}
                style={{
                  flex: 1,
                  padding: '6px 8px',
                  background: 'transparent',
                  border: '1px solid var(--border-2)',
                  borderRadius: 4,
                  color: 'var(--text-2)',
                  cursor: 'pointer',
                  fontSize: 11
                }}
                title={tr('profile.incognitoHint')}
              >
                {tr('profile.incognito')}
              </button>
            </div>

            {browsers.length > 0 && (
              <>
                <div style={{ height: 1, background: 'var(--border-2)', margin: '4px 0' }} />
                <div
                  style={{
                    fontSize: 10,
                    color: 'var(--text-dim)',
                    padding: '4px 8px',
                    textTransform: 'uppercase',
                    letterSpacing: 0.5
                  }}
                >
                  {tr('profile.importFrom')}
                </div>
                {browsers.map((b) => {
                  const expanded = expandedBrowser === b.id
                  return (
                    <div key={b.id}>
                      <button
                        type="button"
                        onClick={() => setExpandedBrowser(expanded ? null : b.id)}
                        style={{
                          display: 'flex',
                          alignItems: 'center',
                          justifyContent: 'space-between',
                          width: '100%',
                          textAlign: 'left',
                          padding: '6px 8px',
                          background: 'transparent',
                          border: 'none',
                          borderRadius: 4,
                          color: 'var(--text)',
                          cursor: 'pointer',
                          fontSize: 12
                        }}
                        title={`Import a ${b.name} profile`}
                      >
                        <span>{b.name}에서</span>
                        <span style={{ color: 'var(--text-dim)', fontSize: 10 }}>
                          {expanded ? '▾' : '▸'}
                        </span>
                      </button>
                      {expanded &&
                        b.profiles.map((p) => (
                          <button
                            key={p.id}
                            type="button"
                            onClick={() => void importBrowser(b, p)}
                            style={{
                              display: 'block',
                              width: '100%',
                              textAlign: 'left',
                              padding: '5px 8px 5px 20px',
                              background: 'transparent',
                              border: 'none',
                              borderRadius: 4,
                              color: 'var(--text-2)',
                              cursor: 'pointer',
                              fontSize: 12
                            }}
                            title={`New profile "${p.name}" seeded with ${b.name} cookies`}
                          >
                            ↓ {p.name}
                          </button>
                        ))}
                    </div>
                  )
                })}
                {importMsg && (
                  <div
                    style={{
                      fontSize: 10,
                      padding: '4px 8px',
                      color: importMsg.startsWith('⚠') ? 'var(--status-error)' : 'var(--text-dim)'
                    }}
                  >
                    {importMsg}
                  </div>
                )}
              </>
            )}
          </div>
          </>
        )}
      </div>
    </div>
  )
}
