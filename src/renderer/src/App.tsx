import { useEffect, useRef, useState, type FormEvent } from 'react'

import { AiActionOverlay } from '@/components/AiActionOverlay'
import { ChatPanel } from '@/components/chat/ChatPanel'
import { TabBar } from '@/components/TabBar'
import { WebviewTab, type WebviewTabHandle } from '@/components/WebviewTab'
import { TrafficList } from '@/components/network/TrafficList'
import { TrafficDetailDrawer } from '@/components/network/TrafficDetailDrawer'
import { useCdpEvents } from '@/hooks/use-cdp-events'
import { useResizable } from '@/hooks/use-resizable'
import { useTabsStore } from '@/stores/tabs'
import { useTrafficStore } from '@/stores/traffic'
import { useViewportStore } from '@/stores/viewport'

function App() {
  useCdpEvents()
  const tabs = useTabsStore((s) => s.tabs)
  const activeId = useTabsStore((s) => s.activeId)
  const addTab = useTabsStore((s) => s.addTab)
  const activeTab = tabs.find((t) => t.id === activeId)

  // window.open / target=_blank from any webview → new tab inside the app.
  useEffect(() => {
    return window.rev.cdp.onNewWindow(({ url }) => {
      if (url) addTab(url)
    })
  }, [addTab])

  const [urlDraft, setUrlDraft] = useState(activeTab?.url ?? '')
  const detailId = useTrafficStore((s) => s.detailId)
  const closeDetail = useTrafficStore((s) => s.closeDetail)
  const viewportMode = useViewportStore((s) => s.mode)
  const setViewportMode = useViewportStore((s) => s.setMode)

  const traffic = useResizable({ initial: 360, min: 240, max: 720, storageKey: 'rev:traffic-w' })
  const detail = useResizable({ initial: 440, min: 320, max: 720, storageKey: 'rev:detail-w' })
  const chat = useResizable({ initial: 420, min: 300, max: 720, storageKey: 'rev:chat-w' })

  const tabRefs = useRef<Map<string, WebviewTabHandle>>(new Map())
  const setTabRef = (id: string) => (h: WebviewTabHandle | null) => {
    if (h) tabRefs.current.set(id, h)
    else tabRefs.current.delete(id)
  }
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
    activeRef()?.loadURL(target)
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
        <form
          onSubmit={onSubmit}
          style={
            {
              display: 'flex',
              flex: 1,
              gap: 6,
              marginLeft: 16,
              WebkitAppRegion: 'no-drag'
            } as React.CSSProperties
          }
        >
          <button type="button" onClick={() => activeRef()?.goBack()} title="Back">
            ←
          </button>
          <button type="button" onClick={() => activeRef()?.goForward()} title="Forward">
            →
          </button>
          <button type="button" onClick={() => activeRef()?.reload()} title="Reload">
            ↻
          </button>
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
        </form>
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
          <TabBar />
          <div style={{ flex: 1, display: 'flex', minHeight: 0, position: 'relative' }}>
            {tabs.map((t) => (
              <WebviewTab
                key={t.id}
                ref={setTabRef(t.id)}
                tab={t}
                active={t.id === activeId}
              />
            ))}
            <AiActionOverlay />
          </div>
        </section>
        <div className="splitter" onMouseDown={traffic.startDrag} title="Resize" />
        <aside
          style={{
            width: traffic.width,
            display: 'flex',
            flexDirection: 'column',
            flexShrink: 0
          }}
        >
          <TrafficList />
        </aside>
        {detailId && (
          <>
            <div className="splitter" onMouseDown={detail.startDrag} title="Resize" />
            <aside
              style={{
                width: detail.width,
                display: 'flex',
                flexDirection: 'column',
                flexShrink: 0
              }}
            >
              <TrafficDetailDrawer requestId={detailId} onClose={closeDetail} />
            </aside>
          </>
        )}
        <div className="splitter" onMouseDown={chat.startDrag} title="Resize" />
        <aside
          style={{
            width: chat.width,
            display: 'flex',
            flexDirection: 'column',
            flexShrink: 0
          }}
        >
          <ChatPanel />
        </aside>
      </main>
    </div>
  )
}

export default App
