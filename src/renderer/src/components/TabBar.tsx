import { useEffect, useRef, useState } from 'react'

import type { BrowserInfo, BrowserProfileInfo } from '../../../preload'
import { useProfilesStore } from '@/stores/profiles'
import { useTabsStore } from '@/stores/tabs'

export function TabBar() {
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

  useEffect(() => {
    void loadProfiles()
    void window.rev.storage.browsers().then(setBrowsers)
  }, [loadProfiles])

  // The menu is position:fixed (not absolute): the tab strip is an overflow:auto
  // container, which clips any dropdown opening below the 40px bar. Fixed
  // positioning escapes that clipping. Anchor it under the toggle button.
  const toggleMenu = (): void => {
    setMenuOpen((v) => {
      if (!v && btnRef.current) {
        const r = btnRef.current.getBoundingClientRect()
        setMenuPos({ top: r.bottom + 4, right: window.innerWidth - r.right })
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

  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'flex-end',
        gap: 2,
        flex: 1,
        minWidth: 0,
        height: '100%',
        overflowX: 'auto'
      }}
    >
      {tabs.map((t) => {
        const isActive = t.id === activeId
        // Ephemeral partition (proxy / incognito) — cookies vanish with the app.
        // Persistent profile tabs (persist:…) render like normal tabs.
        const ephemeral = !!t.partition && !t.partition.startsWith('persist:')
        const label = profileName(t.profileId)
        return (
          <div
            key={t.id}
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
                title="Close tab"
              >
                ×
              </button>
            )}
          </div>
        )
      })}

      <button
        type="button"
        onClick={() => addTab('https://www.google.com')}
        style={
          {
            padding: '4px 10px',
            marginLeft: 4,
            marginBottom: 2,
            background: 'transparent',
            border: '1px solid var(--border-2)',
            borderRadius: 4,
            color: 'var(--text-2)',
            cursor: 'pointer',
            fontSize: 14,
            lineHeight: 1,
            WebkitAppRegion: 'no-drag'
          } as React.CSSProperties
        }
        title="New tab"
      >
        +
      </button>

      <div style={{ marginBottom: 2 }}>
        <button
          ref={btnRef}
          type="button"
          onClick={toggleMenu}
          style={
            {
              padding: '4px 10px',
              background: 'transparent',
              border: '1px solid var(--border-2)',
              borderRadius: 4,
              color: 'var(--text-2)',
              cursor: 'pointer',
              fontSize: 14,
              lineHeight: 1,
              WebkitAppRegion: 'no-drag'
            } as React.CSSProperties
          }
          title="New tab in profile"
        >
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
              Open in profile
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
                    title="Delete profile"
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
              placeholder="New profile name"
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
                title="Create a persistent profile (survives restart)"
              >
                + Persistent
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
                title="Create an incognito profile (in-memory, cleared on quit)"
              >
                + Incognito
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
                  Import profile from browser
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
