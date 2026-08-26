import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react'

import { AiActionOverlay } from '@/components/AiActionOverlay'
import { BookmarkBar } from '@/components/BookmarkBar'
import { BotCheckButton } from '@/components/BotCheckButton'
import { DetailDrawer } from '@/components/DetailDrawer'
import { FloatingChips } from '@/components/FloatingChips'
import { IpBadge } from '@/components/IpBadge'
import { MarkupEditor } from '@/components/MarkupEditor'
import { ProxyButton } from '@/components/ProxyButton'
import { ScreencastView } from '@/components/ScreencastView'
import { TabBar } from '@/components/TabBar'
import { WebviewTab, type WebviewTabHandle } from '@/components/WebviewTab'
import { PermissionPrompt } from '@/components/PermissionPrompt'
import { CopyToast } from '@/components/CopyToast'
import { OnboardingModal } from '@/components/OnboardingModal'
import { ChatPanel } from '@/components/chat/ChatPanel'
import { TerminalPanel } from '@/components/chat/TerminalPanel'
import { requestPermissionFromUser } from '@/ai/acp-permission'
import { useCdpEvents } from '@/hooks/use-cdp-events'
import { useResizable } from '@/hooks/use-resizable'
import { useBrowserModeStore } from '@/stores/browser-mode'
import { useNavigationRequestStore } from '@/stores/navigation-request'
import { useBookmarksStore } from '@/stores/bookmarks'
import { useTabsStore } from '@/stores/tabs'
import { useAppThemeStore, resolveTheme } from '@/stores/app-theme'
import { useAgentModeStore } from '@/stores/agent-mode'
import { useChatCollapsedStore } from '@/stores/chat-collapsed'
import { useChatDraft } from '@/stores/chat-draft'
import { useI18nStore, useT } from '@/stores/i18n'
import { useViewportStore } from '@/stores/viewport'
import { originFromUrl, useWebviewThemeStore, type WebviewTheme } from '@/stores/webview-theme'
import { handleAgentRequest } from '@/workflows/core/agent-bridge'

type PanelId = 'traffic' | 'apimap' | 'console' | 'exceptions' | 'websocket' | 'repeater' | 'storage' | 'history' | 'workflows'

// macOS insets its traffic lights over the top-left of our bar; Windows/Linux
// draw min/max/close as an overlay on the top-right. Reserve space on the
// matching side so the tab strip never slides under the window buttons.
const IS_MAC = typeof navigator !== 'undefined' && navigator.userAgent.includes('Macintosh')

// AI activity floating panel (AiActionOverlay) — off for now, code kept intact.
const SHOW_AI_ACTIVITY_OVERLAY = true

function App() {
  useCdpEvents()
  const tabs = useTabsStore((s) => s.tabs)
  const activeId = useTabsStore((s) => s.activeId)
  const addTab = useTabsStore((s) => s.addTab)
  const activeTab = tabs.find((t) => t.id === activeId)

  const browserMode = useBrowserModeStore((s) => s.mode)
  const setBrowserMode = useBrowserModeStore((s) => s.setMode)

  const bookmarks = useBookmarksStore((s) => s.bookmarks)
  const addBookmark = useBookmarksStore((s) => s.add)
  const removeBookmarkByUrl = useBookmarksStore((s) => s.removeByUrl)
  const isBookmarked = !!activeTab && bookmarks.some((b) => b.url === activeTab.url)

  const themeMode = useAppThemeStore((s) => s.mode)
  const cycleAppTheme = useAppThemeStore((s) => s.cycle)

  // Apply the resolved app theme to <html data-theme> and sync the native
  // titlebar overlay. Re-runs on manual mode change; also listens for OS
  // scheme changes while in 'system' mode.
  useEffect(() => {
    const apply = (): void => {
      const resolved = resolveTheme(themeMode)
      document.documentElement.setAttribute('data-theme', resolved)
      document.documentElement.style.background = resolved === 'dark' ? '#0e0e0e' : '#fbfbfc'
      void window.rev.theme.setTitlebar(resolved)
    }
    apply()
    const mq = window.matchMedia('(prefers-color-scheme: dark)')
    const onChange = (): void => {
      if (useAppThemeStore.getState().mode === 'system') apply()
    }
    mq.addEventListener('change', onChange)
    return () => mq.removeEventListener('change', onChange)
  }, [themeMode])

  // Serve main-process requests for renderer-owned state (saved macros).
  useEffect(() => window.rev.bridge.onRequest(handleAgentRequest), [])

  const themeByOrigin = useWebviewThemeStore((s) => s.byOrigin)
  const cycleTheme = useWebviewThemeStore((s) => s.cycle)
  const activeOrigin = activeTab ? originFromUrl(activeTab.url) : null
  const activeTheme: WebviewTheme = activeOrigin
    ? themeByOrigin[activeOrigin] ?? 'auto'
    : 'auto'

  // window.open / target=_blank / Ctrl-Cmd-middle-click from any webview → new
  // tab inside the app. Ctrl/Cmd/wheel click yields 'background-tab', so open it
  // without stealing focus, matching Chrome; everything else opens foreground.
  useEffect(() => {
    return window.rev.cdp.onNewWindow(({ url, disposition }) => {
      if (url) addTab(url, { activate: disposition !== 'background-tab' })
    })
  }, [addTab])

  // Route agent permission requests from main to the permission queue / UI.
  // No-ops while auto-approve is on (requestPermissionFromUser resolves inline).
  useEffect(() => {
    return window.rev.acp.onPermissionRequest(requestPermissionFromUser)
  }, [])

  const [urlDraft, setUrlDraft] = useState(activeTab?.url ?? '')
  const addressInputRef = useRef<HTMLInputElement>(null)

  // Find-in-page bar (Cmd/Ctrl+F). Acts on the active tab's webview; closes
  // when the user switches tabs.
  const [findOpen, setFindOpen] = useState(false)
  const [findQuery, setFindQuery] = useState('')
  const [findResult, setFindResult] = useState<{ active: number; total: number } | null>(null)
  const findInputRef = useRef<HTMLInputElement>(null)
  const findTabRef = useRef<string | null>(null)
  const viewportMode = useViewportStore((s) => s.mode)
  const setViewportMode = useViewportStore((s) => s.setMode)

  const [openPanel, setOpenPanel] = useState<PanelId | null>(null)

  // Bumped after a proxy change is applied so IpBadge re-checks the egress IP.
  const [ipRefresh, setIpRefresh] = useState(0)

  // Element picker (DevTools-style inspect mode). Main owns the mode via CDP
  // Overlay; this mirrors its state for the toolbar toggle, which also turns
  // itself off after a pick or on Esc.
  const [pickerActive, setPickerActive] = useState(false)
  // Grab: pick an element to capture a screenshot + context. Shares the picker's
  // overlay; `grabbing` tracks the toolbar toggle separately from the picker.
  const [grabbing, setGrabbing] = useState(false)
  const [markupImage, setMarkupImage] = useState<string | null>(null)

  const chat = useResizable({ initial: 420, min: 300, max: 720, storageKey: 'rev:chat-w' })
  const chatCollapsed = useChatCollapsedStore((s) => s.collapsed)
  const setChatCollapsed = useChatCollapsedStore((s) => s.setCollapsed)
  const agentMode = useAgentModeStore((s) => s.mode)
  const setAgentMode = useAgentModeStore((s) => s.setMode)
  const lang = useI18nStore((s) => s.lang)
  const setLang = useI18nStore((s) => s.setLang)
  const t = useT()


  const tabRefs = useRef<Map<string, WebviewTabHandle>>(new Map())
  // Stable ref callback per tab id so React doesn't churn detach/attach
  // every render (which intermittently empties the map between renders).
  const refCallbacks = useRef<Map<string, (h: WebviewTabHandle | null) => void>>(new Map())
  const setTabRef = useCallback((id: string) => {
    let cb = refCallbacks.current.get(id)
    if (!cb) {
      cb = (h: WebviewTabHandle | null) => {
        if (h) tabRefs.current.set(id, h)
        else tabRefs.current.delete(id)
      }
      refCallbacks.current.set(id, cb)
    }
    return cb
  }, [])
  const activeRef = (): WebviewTabHandle | undefined =>
    activeId ? tabRefs.current.get(activeId) : undefined

  // Sync the address bar to whichever tab is active.
  useEffect(() => {
    if (activeTab) setUrlDraft(activeTab.url)
  }, [activeTab?.url, activeTab?.id])

  // When the active tab changes, point CDP's "active target" at it so that
  // AI tool calls (browser_click, etc.) act on the visible tab.
  useEffect(() => {
    if (activeTab?.webContentsId) {
      void window.rev.cdp.setActive(activeTab.webContentsId)
    }
  }, [activeTab?.webContentsId, activeId])

  // Tell main which tab is active so cookie import / sticky-cookie snapshot
  // target this tab's (now isolated) partition.
  useEffect(() => {
    if (activeId) void window.rev.proxy.setActiveTab(activeId)
  }, [activeId])

  useEffect(() => {
    void window.rev.viewport.get().then(setViewportMode)
    const off = window.rev.viewport.onChange(setViewportMode)
    return off
  }, [setViewportMode])

  useEffect(
    () =>
      // picker and grab share the overlay; `mode` says which one is live so the
      // two toggles reflect state independently and both clear on stop.
      window.rev.picker.onState(({ active, mode }) => {
        setPickerActive(active && mode === 'pick')
        setGrabbing(active && mode === 'grab')
      }),
    []
  )

  // A grabbed element: push its context into the chat draft and open the markup
  // editor on its screenshot.
  useEffect(
    () =>
      window.rev.grab.onCaptured((c) => {
        setGrabbing(false)
        const lines = [
          'Grabbed element:',
          c.selector ? `- selector: ${c.selector}` : null,
          c.ref ? `- ref: ${c.ref} (current snapshot only)` : null,
          c.info?.tag ? `- tag: <${c.info.tag}>` : null,
          c.info?.id ? `- id: #${c.info.id}` : null,
          c.info?.text ? `- text: ${c.info.text}` : null
        ].filter(Boolean)
        if (lines.length > 1) useChatDraft.getState().push(lines.join('\n'))
        if (c.dataUrl) setMarkupImage(c.dataUrl)
      }),
    []
  )

  // Esc cancels the picker or grab while the app window has focus. (Main's
  // before-input-event handler covers Esc while the webview has focus.)
  useEffect(() => {
    if (!pickerActive && !grabbing) return
    const onKeyDown = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') void (grabbing ? window.rev.grab.stop() : window.rev.picker.stop())
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [pickerActive, grabbing])

  useEffect(() => {
    const off = window.rev.onReloadRequest(({ ignoreCache }) => {
      // 스토어에서 현재 activeId를 직접 읽어 스테일 클로저 방지
      const id = useTabsStore.getState().activeId
      const handle = id ? tabRefs.current.get(id) : undefined
      handle?.reload(ignoreCache)
    })
    return off
  }, [])

  // ── Find in page ──────────────────────────────────────────────────────────
  const doFind = useCallback(
    (query: string, opts?: { forward?: boolean; findNext?: boolean }) => {
      const id = useTabsStore.getState().activeId
      const handle = tabRefs.current.get(id)
      if (!handle) return
      if (!query) {
        handle.stopFindInPage('clearSelection')
        setFindResult(null)
        return
      }
      handle.findInPage(query, opts)
    },
    []
  )

  const closeFind = useCallback(() => {
    const id = findTabRef.current ?? useTabsStore.getState().activeId
    tabRefs.current.get(id)?.stopFindInPage('clearSelection')
    findTabRef.current = null
    setFindOpen(false)
    setFindResult(null)
  }, [])

  // Live match count from the active tab while the bar is open.
  useEffect(() => {
    if (!findOpen) return
    const handle = activeId ? tabRefs.current.get(activeId) : undefined
    return handle?.onFoundInPage((r) =>
      setFindResult({ active: r.activeMatchOrdinal, total: r.matches })
    )
  }, [findOpen, activeId])

  // Switching tabs closes the bar (and clears highlights on the old tab).
  useEffect(() => {
    if (findOpen && findTabRef.current && activeId !== findTabRef.current) closeFind()
  }, [activeId, findOpen, closeFind])

  // Browser shortcuts forwarded from main (menu accelerators + key
  // interceptors): tab management, tab switching, back/forward, address bar.
  useEffect(() => {
    const focusAddress = (): void => {
      addressInputRef.current?.focus()
      addressInputRef.current?.select()
    }
    return window.rev.onBrowserCommand(({ cmd, index, url }) => {
      const { tabs, activeId, addTab, closeTab, reopenTab, selectTab } = useTabsStore.getState()
      const activeHandle = tabRefs.current.get(activeId)
      switch (cmd) {
        case 'new-tab':
          addTab('about:blank')
          // Focus after the new tab renders, like Chrome's new-tab omnibox.
          setTimeout(focusAddress, 0)
          break
        case 'close-tab':
          if (tabs.length <= 1) window.rev.closeWindow()
          else closeTab(activeId)
          break
        case 'reopen-tab':
          reopenTab()
          break
        case 'next-tab':
        case 'prev-tab': {
          const idx = tabs.findIndex((t) => t.id === activeId)
          if (idx < 0) break
          const delta = cmd === 'next-tab' ? 1 : -1
          selectTab(tabs[(idx + delta + tabs.length) % tabs.length].id)
          break
        }
        case 'select-tab': {
          // Cmd/Ctrl+9 → last tab, 1..8 → nth tab if it exists (Chromium rule).
          const target = index === 9 ? tabs[tabs.length - 1] : tabs[(index ?? 0) - 1]
          if (target) selectTab(target.id)
          break
        }
        case 'back':
          activeHandle?.goBack()
          break
        case 'forward':
          activeHandle?.goForward()
          break
        case 'focus-address':
          focusAddress()
          break
        case 'find':
          if (useBrowserModeStore.getState().mode !== 'embedded') break
          findTabRef.current = activeId
          setFindOpen(true)
          setTimeout(() => {
            findInputRef.current?.focus()
            findInputRef.current?.select()
          }, 0)
          break
        case 'open-tab':
          if (url) addTab(url)
          break
      }
    })
  }, [])

  // Launch / teardown external Chrome when mode switches
  useEffect(() => {
    if (browserMode === 'external') {
      void window.rev.external.start().catch((e) => {
        console.error('[external] start failed:', e)
      })
    } else {
      void window.rev.external.stop().catch(() => {})
    }
  }, [browserMode])

  // Handle navigation requests from other components (e.g. HistoryPanel).
  const pendingNav = useNavigationRequestStore((s) => s.pending)
  const clearPendingNav = useNavigationRequestStore((s) => s.clear)
  useEffect(() => {
    if (!pendingNav) return
    const { url } = pendingNav
    setUrlDraft(url)
    if (browserMode === 'external') {
      void window.rev.external.navigate(url).catch(() => {})
    } else {
      activeRef()?.loadURL(url)
    }
    clearPendingNav()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingNav, browserMode])

  const onToggleViewport = async () => {
    const next = viewportMode === 'desktop' ? 'mobile' : 'desktop'
    try {
      await window.rev.viewport.set(next)
    } catch (e) {
      console.error('[viewport] set failed:', e)
    }
  }

  const openViewSource = () => {
    if (!activeTab) return
    if (!/^https?:\/\//i.test(activeTab.url)) return
    addTab(`view-source:${activeTab.url}`)
  }

  const onSubmit = (e: FormEvent) => {
    e.preventDefault()
    let target = urlDraft.trim()
    if (!target) return
    if (!/^(https?|view-source):/i.test(target)) target = 'https://' + target

    if (browserMode === 'external') {
      void window.rev.external.navigate(target).catch((err) => {
        console.error('[address-bar] external navigate failed:', err)
      })
      return
    }

    const handle = activeRef()
    if (!handle) {
      console.warn('[address-bar] no active webview ref', { activeId, mapKeys: [...tabRefs.current.keys()] })
      return
    }
    handle.loadURL(target)
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100vh' }}>
      <OnboardingModal />
      <PermissionPrompt />
      <CopyToast />
      {markupImage && (
        <MarkupEditor imageDataUrl={markupImage} onClose={() => setMarkupImage(null)} />
      )}
      <div
        style={
          {
            display: 'flex',
            alignItems: 'flex-end',
            height: 40,
            flexShrink: 0,
            background: 'var(--bg-bar)',
            borderBottom: '1px solid var(--border)',
            paddingLeft: IS_MAC ? 78 : 8,
            paddingRight: IS_MAC ? 8 : 140,
            WebkitAppRegion: 'drag'
          } as React.CSSProperties
        }
      >
        {browserMode === 'embedded' ? (
          <TabBar />
        ) : (
          <span style={{ flex: 1, fontSize: 12, opacity: 0.6, marginBottom: 11 }}>
            rever-browser — External Chrome
          </span>
        )}
        <IpBadge refreshSignal={ipRefresh} />
        <button
          className="toolbar-btn"
          type="button"
          onClick={() => cycleAppTheme()}
          title={t('toolbar.theme', { mode: themeMode })}
          style={
            {
              flexShrink: 0,
              marginLeft: 8,
              marginBottom: 6,
              WebkitAppRegion: 'no-drag'
            } as React.CSSProperties
          }
        >
          <span style={{ fontSize: 11, fontWeight: 500 }}>{t(`toolbar.theme.${themeMode}`)}</span>
        </button>
        <button
          className="toolbar-btn"
          type="button"
          onClick={() => setLang(lang === 'en' ? 'ko' : 'en')}
          title={t('lang.label')}
          style={
            {
              flexShrink: 0,
              marginLeft: 4,
              marginBottom: 6,
              fontSize: 11,
              WebkitAppRegion: 'no-drag'
            } as React.CSSProperties
          }
        >
          {lang === 'en' ? 'EN' : '한'}
        </button>
      </div>

      <main style={{ flex: 1, display: 'flex', minHeight: 0, position: 'relative' }}>
        <section
          style={{
            flex: 1,
            display: 'flex',
            flexDirection: 'column',
            minWidth: 0,
            position: 'relative'
          }}
        >
          <form
            onSubmit={onSubmit}
            style={{
              display: 'flex',
              gap: 6,
              padding: '6px 10px',
              borderBottom: '1px solid var(--border)',
              alignItems: 'center'
            }}
          >
            {browserMode === 'embedded' && (
              <>
                <button
                  className="toolbar-btn"
                  type="button"
                  onClick={() => activeRef()?.goBack()}
                  title="Back"
                >
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <path d="M15 18l-6-6 6-6" />
                  </svg>
                </button>
                <button
                  className="toolbar-btn"
                  type="button"
                  onClick={() => activeRef()?.goForward()}
                  title="Forward"
                >
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <path d="M9 18l6-6-6-6" />
                  </svg>
                </button>
                <button
                  className="toolbar-btn"
                  type="button"
                  onClick={() => activeRef()?.reload(true)}
                  title="Hard Reload (clear cache)"
                >
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <polyline points="23 4 23 10 17 10" />
                    <path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10" />
                  </svg>
                </button>
                <button
                  className="toolbar-btn"
                  type="button"
                  onClick={openViewSource}
                  disabled={!activeTab || !/^https?:\/\//i.test(activeTab.url)}
                  title={t('toolbar.viewSource')}
                  style={{ fontFamily: 'ui-monospace, monospace' }}
                >
                  &lt;/&gt;
                </button>
                <button
                  className="toolbar-btn"
                  type="button"
                  aria-pressed={pickerActive}
                  onClick={() => {
                    void (pickerActive ? window.rev.picker.stop() : window.rev.picker.start())
                  }}
                  disabled={!activeTab}
                  title={t('toolbar.pick')}
                  style={{
                    background: pickerActive ? 'var(--accent-soft)' : undefined,
                    borderColor: pickerActive ? 'var(--accent-border)' : undefined
                  }}
                >
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <circle cx="12" cy="12" r="7" />
                    <line x1="12" y1="2" x2="12" y2="6" />
                    <line x1="12" y1="18" x2="12" y2="22" />
                    <line x1="2" y1="12" x2="6" y2="12" />
                    <line x1="18" y1="12" x2="22" y2="12" />
                  </svg>
                </button>
                <button
                  className="toolbar-btn"
                  type="button"
                  aria-pressed={grabbing}
                  onClick={() => {
                    if (grabbing) {
                      void window.rev.grab.stop()
                      setGrabbing(false)
                    } else {
                      void window.rev.grab.start()
                      setGrabbing(true)
                    }
                  }}
                  disabled={!activeTab}
                  title={t('toolbar.grab')}
                  style={{
                    background: grabbing ? 'var(--accent-soft)' : undefined,
                    borderColor: grabbing ? 'var(--accent-border)' : undefined
                  }}
                >
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <path d="M5 3v4M3 5h4M6 17v4M4 19h4M13 3l2.5 6.5L22 12l-6.5 2.5L13 21l-2.5-6.5L4 12l6.5-2.5z" />
                  </svg>
                </button>
              </>
            )}
            <input
              ref={addressInputRef}
              value={urlDraft}
              onChange={(e) => setUrlDraft(e.target.value)}
              placeholder={t('toolbar.addressPlaceholder')}
              style={{
                flex: 1,
                height: 28,
                padding: '0 10px',
                fontFamily: 'ui-monospace, monospace',
                boxSizing: 'border-box'
              }}
            />
            {browserMode === 'embedded' && (
              <button
                className="toolbar-btn"
                type="button"
                onClick={() => {
                  if (!activeTab) return
                  if (isBookmarked) removeBookmarkByUrl(activeTab.url)
                  else addBookmark({ url: activeTab.url, title: activeTab.title })
                }}
                disabled={!activeTab || !/^https?:\/\//i.test(activeTab.url)}
                title={isBookmarked ? 'Remove bookmark' : 'Bookmark this page'}
              >
                <svg
                  width="15"
                  height="15"
                  viewBox="0 0 24 24"
                  fill={isBookmarked ? 'var(--accent)' : 'none'}
                  stroke={isBookmarked ? 'var(--accent)' : 'currentColor'}
                  strokeWidth={2}
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden="true"
                >
                  <polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2" />
                </svg>
              </button>
            )}
            <button className="toolbar-btn" type="submit">Go</button>
            <button
              className="toolbar-btn"
              type="button"
              onClick={() => {
                if (activeOrigin) cycleTheme(activeOrigin)
              }}
              disabled={!activeOrigin}
              title={
                activeOrigin
                  ? `Page theme for ${activeOrigin} — Auto follows the app theme. Click to cycle Auto → Light → Dark`
                  : 'No site loaded'
              }
              style={{
                background: activeTheme === 'light'
                  ? '#eee'
                  : activeTheme === 'dark'
                    ? '#222'
                    : undefined,
                color: activeTheme === 'light' ? '#111' : activeTheme === 'dark' ? '#eee' : undefined,
                borderColor:
                  activeTheme === 'light'
                    ? '#aaa'
                    : activeTheme === 'dark'
                      ? '#555'
                      : undefined
              }}
            >
              {activeTheme === 'auto' ? 'Auto' : activeTheme === 'light' ? 'Light' : 'Dark'}
            </button>
            {browserMode === 'embedded' && (
              <ProxyButton
                tab={activeTab}
                onApplied={() => {
                  activeRef()?.reload()
                  setIpRefresh((n) => n + 1)
                }}
              />
            )}
            <button
              className="toolbar-btn"
              type="button"
              onClick={onToggleViewport}
              title="Toggle desktop/mobile viewport"
              style={{
                background: viewportMode === 'mobile' ? '#244' : undefined,
                color: viewportMode === 'mobile' ? '#dee' : undefined,
                borderColor: viewportMode === 'mobile' ? '#377' : undefined
              }}
            >
              {viewportMode === 'mobile' ? t('toolbar.viewport.mobile') : t('toolbar.viewport.desktop')}
            </button>
            <div
              role="group"
              aria-label="Browser mode"
              title="Embedded runs pages in the in-app webview; External attaches to a real Chrome window"
              style={{ display: 'inline-flex', flexShrink: 0 }}
            >
              <button
                className="toolbar-btn"
                type="button"
                aria-pressed={browserMode === 'embedded'}
                onClick={() => setBrowserMode('embedded')}
                style={{
                  borderTopRightRadius: 0,
                  borderBottomRightRadius: 0,
                  background: browserMode === 'embedded' ? 'var(--accent-soft)' : undefined,
                  borderColor: browserMode === 'embedded' ? 'var(--accent-border)' : undefined
                }}
              >
                {t('toolbar.embedded')}
              </button>
              <button
                className="toolbar-btn"
                type="button"
                aria-pressed={browserMode === 'external'}
                onClick={() => setBrowserMode('external')}
                style={{
                  marginLeft: -1,
                  borderTopLeftRadius: 0,
                  borderBottomLeftRadius: 0,
                  background: browserMode === 'external' ? 'var(--accent-soft)' : undefined,
                  borderColor: browserMode === 'external' ? 'var(--accent-border)' : undefined
                }}
              >
                {t('toolbar.external')}
              </button>
            </div>
          </form>
          {browserMode === 'embedded' && <BookmarkBar />}
          {/* paddingBottom reserves the chip bar's strip so the webview ends
              above it instead of being covered by it. The bar and the slide-up
              panel are absolutely positioned against this box's bottom edge,
              which is the padding edge — so they still sit flush. */}
          <div
            style={{
              flex: 1,
              position: 'relative',
              display: 'flex',
              minHeight: 0,
              paddingBottom: 'var(--chip-bar-h)'
            }}
          >
            {browserMode === 'embedded' ? (
              <>
                {tabs.map((t) => (
                  <WebviewTab
                    key={t.id}
                    ref={setTabRef(t.id)}
                    tab={t}
                    active={t.id === activeId}
                  />
                ))}
              </>
            ) : (
              <ScreencastView />
            )}
            {findOpen && (
              <div
                style={{
                  position: 'absolute',
                  top: 8,
                  right: 12,
                  zIndex: 30,
                  display: 'flex',
                  gap: 4,
                  alignItems: 'center',
                  padding: '5px 6px',
                  background: 'var(--bg-bar)',
                  border: '1px solid var(--border)',
                  borderRadius: 6,
                  boxShadow: '0 2px 10px rgba(0,0,0,0.25)'
                }}
              >
                <input
                  ref={findInputRef}
                  value={findQuery}
                  onChange={(e) => {
                    setFindQuery(e.target.value)
                    doFind(e.target.value)
                  }}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') {
                      e.preventDefault()
                      doFind(findQuery, { findNext: true, forward: !e.shiftKey })
                    } else if (e.key === 'Escape') {
                      e.preventDefault()
                      closeFind()
                    }
                  }}
                  placeholder="Find in page"
                  style={{ width: 180, height: 24, padding: '0 8px', boxSizing: 'border-box' }}
                />
                <span
                  style={{
                    fontSize: 11,
                    color: 'var(--text-dim)',
                    minWidth: 42,
                    textAlign: 'center'
                  }}
                >
                  {findQuery ? `${findResult?.active ?? 0}/${findResult?.total ?? 0}` : ''}
                </span>
                <button
                  className="toolbar-btn"
                  type="button"
                  title="Previous match (Shift+Enter)"
                  onClick={() => doFind(findQuery, { findNext: true, forward: false })}
                >
                  ↑
                </button>
                <button
                  className="toolbar-btn"
                  type="button"
                  title="Next match (Enter)"
                  onClick={() => doFind(findQuery, { findNext: true, forward: true })}
                >
                  ↓
                </button>
                <button className="toolbar-btn" type="button" title="Close (Esc)" onClick={closeFind}>
                  ✕
                </button>
              </div>
            )}
            {/* AI activity floating panel — hidden for now, feature kept.
                Re-enable by flipping SHOW_AI_ACTIVITY_OVERLAY. */}
            {SHOW_AI_ACTIVITY_OVERLAY && <AiActionOverlay />}
            <BotCheckButton onNavigate={(url) => {
              if (browserMode === 'external') {
                void window.rev.external.navigate(url).catch(() => {})
              } else {
                activeRef()?.loadURL(url)
              }
            }} />
            <FloatingChips openPanel={openPanel} setOpenPanel={setOpenPanel} />
            <DetailDrawer />
          </div>
        </section>

        {!chatCollapsed && (
          <div className="splitter" onMouseDown={chat.startDrag} title="Drag to resize" />
        )}
        <button
          type="button"
          className="chat-toggle-handle"
          style={{ right: chatCollapsed ? 2 : chat.width + 4 }}
          onClick={() => setChatCollapsed(!chatCollapsed)}
          title={chatCollapsed ? t('toolbar.openChat') : t('toolbar.collapseChat')}
          aria-label={chatCollapsed ? t('toolbar.openChat') : t('toolbar.collapseChat')}
        >
          {chatCollapsed ? '‹' : '›'}
        </button>

        {/* Kept mounted while collapsed (width 0 + hidden overflow) so the chat
            session and any in-flight stream survive the toggle. */}
        <aside
          className="agent-panel"
          style={{
            width: chatCollapsed ? 0 : chat.width,
            flexShrink: 0,
            display: 'flex',
            flexDirection: 'column',
            minHeight: 0,
            overflow: 'hidden',
            borderLeft: chatCollapsed ? 'none' : '1px solid var(--border)',
            ['--chat-w' as never]: `${chat.width}px`
          }}
        >
          <div
            style={{
              display: 'flex',
              gap: 4,
              padding: '6px 8px',
              borderBottom: '1px solid var(--border)',
              flexShrink: 0
            }}
          >
            <AgentModeTab label={t('agent.chat')} active={agentMode === 'acp'} onClick={() => setAgentMode('acp')} />
            <AgentModeTab label={t('agent.terminal')} active={agentMode === 'cli'} onClick={() => setAgentMode('cli')} />
          </div>
          {/* Keep ChatPanel mounted (preserves ACP session) but hidden in CLI
              mode; mount the terminal only while selected so the PTY is killed
              when you switch away. */}
          <div style={{ flex: 1, minHeight: 0, display: agentMode === 'acp' ? 'flex' : 'none', flexDirection: 'column' }}>
            <ChatPanel />
          </div>
          {agentMode === 'cli' && (
            <div style={{ flex: 1, minHeight: 0 }}>
              <TerminalPanel />
            </div>
          )}
        </aside>

      </main>
    </div>
  )
}

function AgentModeTab({
  label,
  active,
  onClick
}: {
  label: string
  active: boolean
  onClick: () => void
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      style={{
        flex: 1,
        padding: '5px 8px',
        fontSize: 11,
        background: active ? 'var(--accent-soft)' : 'transparent',
        border: `1px solid ${active ? 'var(--accent-border)' : 'var(--border-2)'}`,
        borderRadius: 4,
        color: active ? 'var(--accent)' : 'var(--text-2)',
        cursor: 'pointer'
      }}
    >
      {label}
    </button>
  )
}

export default App
