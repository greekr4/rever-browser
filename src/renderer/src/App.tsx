import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react'

import { AiActionOverlay } from '@/components/AiActionOverlay'
import { BotCheckButton } from '@/components/BotCheckButton'
import { DetailDrawer } from '@/components/DetailDrawer'
import { FloatingChips } from '@/components/FloatingChips'
import { ScreencastView } from '@/components/ScreencastView'
import { TabBar } from '@/components/TabBar'
import { WebviewTab, type WebviewTabHandle } from '@/components/WebviewTab'
import { ChatPanel } from '@/components/chat/ChatPanel'
import { useCdpEvents } from '@/hooks/use-cdp-events'
import { useResizable } from '@/hooks/use-resizable'
import { useBrowserModeStore } from '@/stores/browser-mode'
import { useTabsStore } from '@/stores/tabs'
import { useViewportStore } from '@/stores/viewport'

type PanelId = 'traffic' | 'console' | 'exceptions' | 'websocket'

function App() {
  useCdpEvents()
  const tabs = useTabsStore((s) => s.tabs)
  const activeId = useTabsStore((s) => s.activeId)
  const addTab = useTabsStore((s) => s.addTab)
  const activeTab = tabs.find((t) => t.id === activeId)

  const browserMode = useBrowserModeStore((s) => s.mode)
  const setBrowserMode = useBrowserModeStore((s) => s.setMode)

  // window.open / target=_blank from any webview → new tab inside the app.
  useEffect(() => {
    return window.rev.cdp.onNewWindow(({ url }) => {
      if (url) addTab(url)
    })
  }, [addTab])

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
      activeRef()?.reload(ignoreCache)
    })
    return off
    // eslint-disable-next-line react-hooks/exhaustive-deps
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

  const onToggleViewport = async () => {
    const next = viewportMode === 'desktop' ? 'mobile' : 'desktop'
    try {
      await window.rev.viewport.set(next)
    } catch (e) {
      console.error('[viewport] set failed:', e)
    }
  }

  const onSubmit = (e: FormEvent) => {
    e.preventDefault()
    let target = urlDraft.trim()
    if (!target) return
    if (!/^https?:\/\//i.test(target)) target = 'https://' + target

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
    console.log('[address-bar] loadURL', target)
    handle.loadURL(target)
  }

  const anyAttached = tabs.some((t) => t.webContentsId)

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100vh' }}>
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
        <strong style={{ fontSize: 13, marginLeft: 60 }}>rever-browser</strong>
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
                <button type="button" onClick={() => activeRef()?.goBack()} title="Back">
                  ←
                </button>
                <button type="button" onClick={() => activeRef()?.goForward()} title="Forward">
                  →
                </button>
                <button type="button" onClick={() => activeRef()?.reload()} title="Reload">
                  ↻
                </button>
                <button
                  type="button"
                  onClick={() => activeRef()?.reload(true)}
                  title="Hard Reload (clear cache)"
                >
                  ⟳
                </button>
              </>
            )}
            <input
              value={urlDraft}
              onChange={(e) => setUrlDraft(e.target.value)}
              placeholder="https://..."
              style={{ flex: 1, padding: '4px 10px', fontFamily: 'ui-monospace, monospace' }}
            />
            <button type="submit">Go</button>
            <button
              type="button"
              onClick={onToggleViewport}
              title="Toggle desktop/mobile viewport"
              style={{
                fontSize: 11,
                background: viewportMode === 'mobile' ? '#244' : undefined,
                borderColor: viewportMode === 'mobile' ? '#377' : undefined
              }}
            >
              {viewportMode === 'mobile' ? 'Mobile' : 'Desktop'}
            </button>
            <button
              type="button"
              onClick={() => setBrowserMode(browserMode === 'embedded' ? 'external' : 'embedded')}
              title="Toggle embedded/external Chrome"
              style={{
                fontSize: 11,
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
            borderLeft: '1px solid #2a2a2a'
          }}
        >
          <ChatPanel />
        </aside>
      </main>
    </div>
  )
}

export default App
