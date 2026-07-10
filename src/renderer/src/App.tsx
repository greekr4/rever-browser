import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react'

import { AiActionOverlay } from '@/components/AiActionOverlay'
import { BotCheckButton } from '@/components/BotCheckButton'
import { DetailDrawer } from '@/components/DetailDrawer'
import { FloatingChips } from '@/components/FloatingChips'
import { ScreencastView } from '@/components/ScreencastView'
import { TabBar } from '@/components/TabBar'
import { WebviewTab, type WebviewTabHandle } from '@/components/WebviewTab'
import { PermissionPrompt } from '@/components/PermissionPrompt'
import { ChatPanel } from '@/components/chat/ChatPanel'
import { requestPermissionFromUser } from '@/ai/acp-permission'
import { useCdpEvents } from '@/hooks/use-cdp-events'
import { useResizable } from '@/hooks/use-resizable'
import { useBrowserModeStore } from '@/stores/browser-mode'
import { useNavigationRequestStore } from '@/stores/navigation-request'
import { useTabsStore } from '@/stores/tabs'
import { useViewportStore } from '@/stores/viewport'
import { originFromUrl, useWebviewThemeStore, type WebviewTheme } from '@/stores/webview-theme'

type PanelId = 'traffic' | 'console' | 'exceptions' | 'websocket' | 'repeater' | 'storage' | 'history'

function App() {
  useCdpEvents()
  const tabs = useTabsStore((s) => s.tabs)
  const activeId = useTabsStore((s) => s.activeId)
  const addTab = useTabsStore((s) => s.addTab)
  const activeTab = tabs.find((t) => t.id === activeId)

  const browserMode = useBrowserModeStore((s) => s.mode)
  const setBrowserMode = useBrowserModeStore((s) => s.setMode)

  const themeByOrigin = useWebviewThemeStore((s) => s.byOrigin)
  const cycleTheme = useWebviewThemeStore((s) => s.cycle)
  const activeOrigin = activeTab ? originFromUrl(activeTab.url) : null
  const activeTheme: WebviewTheme = activeOrigin
    ? themeByOrigin[activeOrigin] ?? 'auto'
    : 'auto'

  // window.open / target=_blank from any webview → new tab inside the app.
  useEffect(() => {
    return window.rev.cdp.onNewWindow(({ url }) => {
      if (url) addTab(url)
    })
  }, [addTab])

  // Route agent permission requests from main to the permission queue / UI.
  // No-ops while auto-approve is on (requestPermissionFromUser resolves inline).
  useEffect(() => {
    return window.rev.acp.onPermissionRequest(requestPermissionFromUser)
  }, [])

  const [urlDraft, setUrlDraft] = useState(activeTab?.url ?? '')
  const viewportMode = useViewportStore((s) => s.mode)
  const setViewportMode = useViewportStore((s) => s.setMode)

  const [openPanel, setOpenPanel] = useState<PanelId | null>(null)

  const chat = useResizable({ initial: 420, min: 300, max: 720, storageKey: 'rev:chat-w' })

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

  useEffect(() => {
    void window.rev.viewport.get().then(setViewportMode)
    const off = window.rev.viewport.onChange(setViewportMode)
    return off
  }, [setViewportMode])

  useEffect(() => {
    const off = window.rev.onReloadRequest(({ ignoreCache }) => {
      // 스토어에서 현재 activeId를 직접 읽어 스테일 클로저 방지
      const id = useTabsStore.getState().activeId
      const handle = id ? tabRefs.current.get(id) : undefined
      handle?.reload(ignoreCache)
    })
    return off
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

  const anyAttached = tabs.some((t) => t.webContentsId)

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100vh' }}>
      <PermissionPrompt />
      <header
        style={
          {
            display: 'flex',
            gap: 8,
            alignItems: 'center',
            padding: '10px 14px',
            borderBottom: '1px solid #2a2a2a',
            WebkitAppRegion: 'drag'
          } as React.CSSProperties
        }
      >
        <strong style={{ fontSize: 13, marginLeft: 80 }}>rever-browser</strong>
        <span style={{ fontSize: 11, opacity: 0.6 }}>
          {anyAttached ? '● CDP attached' : '○ attaching…'}
        </span>
      </header>

      <main style={{ flex: 1, display: 'flex', minHeight: 0 }}>
        <section
          style={{
            flex: 1,
            display: 'flex',
            flexDirection: 'column',
            minWidth: 0,
            position: 'relative'
          }}
        >
          {browserMode === 'embedded' && <TabBar />}
          <form
            onSubmit={onSubmit}
            style={{
              display: 'flex',
              gap: 6,
              padding: '6px 10px',
              borderBottom: '1px solid #2a2a2a',
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
                  ←
                </button>
                <button
                  className="toolbar-btn"
                  type="button"
                  onClick={() => activeRef()?.goForward()}
                  title="Forward"
                >
                  →
                </button>
                <button
                  className="toolbar-btn"
                  type="button"
                  onClick={() => activeRef()?.reload(true)}
                  title="Hard Reload (clear cache)"
                >
                  ↻
                </button>
                <button
                  className="toolbar-btn"
                  type="button"
                  onClick={openViewSource}
                  disabled={!activeTab || !/^https?:\/\//i.test(activeTab.url)}
                  title="View page source in a new tab (view-source:)"
                  style={{ fontFamily: 'ui-monospace, monospace' }}
                >
                  &lt;/&gt;
                </button>
              </>
            )}
            <input
              value={urlDraft}
              onChange={(e) => setUrlDraft(e.target.value)}
              placeholder="https://..."
              style={{
                flex: 1,
                height: 28,
                padding: '0 10px',
                fontFamily: 'ui-monospace, monospace',
                boxSizing: 'border-box'
              }}
            />
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
                  ? `Page theme for ${activeOrigin} — click to cycle Auto → Light → Dark`
                  : 'No site loaded'
              }
              style={{
                background: activeTheme === 'light'
                  ? '#eee'
                  : activeTheme === 'dark'
                    ? '#222'
                    : undefined,
                color: activeTheme === 'light' ? '#111' : undefined,
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
            <button
              className="toolbar-btn"
              type="button"
              onClick={onToggleViewport}
              title="Toggle desktop/mobile viewport"
              style={{
                background: viewportMode === 'mobile' ? '#244' : undefined,
                borderColor: viewportMode === 'mobile' ? '#377' : undefined
              }}
            >
              {viewportMode === 'mobile' ? 'Mobile' : 'Desktop'}
            </button>
            <button
              className="toolbar-btn"
              type="button"
              onClick={() => setBrowserMode(browserMode === 'embedded' ? 'external' : 'embedded')}
              title="Toggle embedded/external Chrome"
              style={{
                background: browserMode === 'external' ? '#242' : undefined,
                borderColor: browserMode === 'external' ? '#373' : undefined
              }}
            >
              {browserMode === 'embedded' ? 'Embedded' : 'External (real Chrome)'}
            </button>
          </form>
          <div style={{ flex: 1, position: 'relative', display: 'flex', minHeight: 0 }}>
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
            <AiActionOverlay />
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

        <div className="splitter" onMouseDown={chat.startDrag} />

        <aside
          className="agent-panel"
          style={{
            width: chat.width,
            flexShrink: 0,
            display: 'flex',
            flexDirection: 'column',
            minHeight: 0,
            borderLeft: '1px solid #2a2a2a',
            ['--chat-w' as never]: `${chat.width}px`
          }}
        >
          <ChatPanel />
        </aside>
      </main>
    </div>
  )
}

export default App
