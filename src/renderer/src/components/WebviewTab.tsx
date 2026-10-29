import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from 'react'

import { useHistoryStore } from '@/stores/history'
import type { Tab } from '@/stores/tabs'
import { useTabsStore } from '@/stores/tabs'
import { useScanStore } from '@/stores/scan'
import { useAppThemeStore } from '@/stores/app-theme'
import {
  originFromUrl,
  resolveWebviewTheme,
  useWebviewThemeStore
} from '@/stores/webview-theme'

interface Props {
  tab: Tab
  active: boolean
  // Red Team grid view: show every tab as a tile while a scan runs.
  tiled?: boolean
}

export interface WebviewTabHandle {
  loadURL: (url: string) => void
  goBack: () => void
  goForward: () => void
  reload: (ignoreCache?: boolean) => void
  findInPage: (text: string, opts?: { forward?: boolean; findNext?: boolean }) => void
  stopFindInPage: (action?: 'clearSelection' | 'keepSelection') => void
  // Subscribe to find-result updates; returns an unsubscribe function.
  onFoundInPage: (cb: (r: { activeMatchOrdinal: number; matches: number }) => void) => () => void
}

interface PageTitleEvent extends Event {
  title: string
}

export const WebviewTab = forwardRef<WebviewTabHandle, Props>(function WebviewTab(
  { tab, active, tiled },
  ref
) {
  const wvRef = useRef<Electron.WebviewTag>(null)
  const updateTab = useTabsStore((s) => s.updateTab)

  useImperativeHandle(
    ref,
    () => ({
      loadURL: (url) => {
        const wv = wvRef.current
        if (!wv) {
          console.warn('[webview] loadURL skipped: ref is null', url)
          return
        }
        // isLoading() throws or returns undefined before dom-ready/attach.
        // In that case defer until dom-ready fires.
        let ready = false
        try {
          ready = wv.isLoading?.() !== undefined
        } catch {
          ready = false
        }
        if (!ready) {
          const onReady = () => {
            wv.removeEventListener('dom-ready', onReady)
            wv.loadURL(url).catch((e) => console.error('[webview] loadURL failed', e))
          }
          wv.addEventListener('dom-ready', onReady)
          return
        }
        wv.loadURL(url).catch((e) => console.error('[webview] loadURL failed', e))
      },
      goBack: () => wvRef.current?.goBack(),
      goForward: () => wvRef.current?.goForward(),
      reload: (ignoreCache = false) => {
        const wv = wvRef.current
        if (!wv) return
        ignoreCache ? wv.reloadIgnoringCache() : wv.reload()
      },
      findInPage: (text, opts) => {
        wvRef.current?.findInPage(text, opts)
      },
      stopFindInPage: (action = 'clearSelection') => {
        wvRef.current?.stopFindInPage(action)
      },
      onFoundInPage: (cb) => {
        const wv = wvRef.current
        if (!wv) return () => {}
        const listener = (e: Electron.FoundInPageEvent) =>
          cb({ activeMatchOrdinal: e.result.activeMatchOrdinal, matches: e.result.matches })
        wv.addEventListener('found-in-page', listener)
        return () => wv.removeEventListener('found-in-page', listener)
      }
    }),
    []
  )

  useEffect(() => {
    const wv = wvRef.current
    if (!wv) return
    let attached = false
    // tryAttach 성공 시 실제 id를 저장해 cleanup에서 올바르게 detach
    let attachedId: number | null = null

    const tryAttach = async () => {
      if (attached) return
      let id: number
      try {
        id = wv.getWebContentsId()
      } catch {
        return
      }
      if (!id || id <= 0) return
      const ok = await window.rev.cdp.attach(id)
      if (ok) {
        attached = true
        attachedId = id
        updateTab(tab.id, { webContentsId: id })
        void applyTheme()
      }
    }

    const pushHistory = useHistoryStore.getState().push
    const updateHistoryTitle = useHistoryStore.getState().updateTitle

    // Emulate `prefers-color-scheme` via CDP so the site serves its own
    // light/dark stylesheet. Needs the debugger attached, so this is a no-op
    // until tryAttach succeeds (which then calls it).
    const applyTheme = async () => {
      if (attachedId == null) return
      const url = useTabsStore.getState().tabs.find((t) => t.id === tab.id)?.url
      const origin = url ? originFromUrl(url) : null
      const pref = origin ? useWebviewThemeStore.getState().get(origin) : 'auto'
      await window.rev.theme.setWebviewScheme(attachedId, resolveWebviewTheme(pref))
    }

    const onNavigate = (e: Electron.DidNavigateEvent) => {
      updateTab(tab.id, { url: e.url })
      pushHistory({ tabId: tab.id, url: e.url, title: '' })
      void applyTheme()
    }
    const onNavigateInPage = (e: Electron.DidNavigateInPageEvent) => {
      if (!e.isMainFrame) return
      updateTab(tab.id, { url: e.url })
      pushHistory({ tabId: tab.id, url: e.url, title: '' })
      void applyTheme()
    }
    const onTitle = (e: Event) => {
      const ev = e as PageTitleEvent
      if (ev.title) {
        updateTab(tab.id, { title: ev.title })
        const currentUrl = useTabsStore.getState().tabs.find((t) => t.id === tab.id)?.url
        if (currentUrl) updateHistoryTitle(tab.id, currentUrl, ev.title)
      }
    }

    const onDomReady = () => {
      void applyTheme()
    }

    const unsubTheme = useWebviewThemeStore.subscribe((state, prev) => {
      const currentUrl = useTabsStore.getState().tabs.find((t) => t.id === tab.id)?.url
      const origin = currentUrl ? originFromUrl(currentUrl) : null
      if (!origin) return
      if (state.byOrigin[origin] === prev.byOrigin[origin]) return
      void applyTheme()
    })

    // 'auto' follows the app theme, so app-theme changes must repaint the page.
    const unsubAppTheme = useAppThemeStore.subscribe((state, prev) => {
      if (state.mode === prev.mode) return
      void applyTheme()
    })
    // …and while the app theme is 'system', an OS switch changes it too.
    const mq = window.matchMedia('(prefers-color-scheme: dark)')
    const onSystemScheme = () => {
      if (useAppThemeStore.getState().mode === 'system') void applyTheme()
    }
    mq.addEventListener('change', onSystemScheme)

    wv.addEventListener('dom-ready', tryAttach)
    wv.addEventListener('dom-ready', onDomReady)
    wv.addEventListener('did-attach', tryAttach)
    wv.addEventListener('did-finish-load', tryAttach)
    wv.addEventListener('did-navigate', onNavigate)
    wv.addEventListener('did-navigate-in-page', onNavigateInPage)
    wv.addEventListener('page-title-updated', onTitle)

    return () => {
      wv.removeEventListener('dom-ready', tryAttach)
      wv.removeEventListener('dom-ready', onDomReady)
      wv.removeEventListener('did-attach', tryAttach)
      wv.removeEventListener('did-finish-load', tryAttach)
      wv.removeEventListener('did-navigate', onNavigate)
      wv.removeEventListener('did-navigate-in-page', onNavigateInPage)
      wv.removeEventListener('page-title-updated', onTitle)
      unsubTheme()
      unsubAppTheme()
      mq.removeEventListener('change', onSystemScheme)

      // Best-effort detach when the tab unmounts (closed).
      // attachedId는 tryAttach 성공 시 기록한 실제 id (tab.webContentsId는 마운트 시점 null일 수 있음)
      if (attachedId) void window.rev.cdp.detach(attachedId)
    }
    // tab.id is stable for a tab's lifetime; don't depend on tab.url which
    // would tear down listeners on every navigation.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab.id])

  // Tile: render the page at a real viewport size, then scale it down so the
  // WHOLE page shows as a thumbnail at the same aspect ratio (not a clipped
  // top-left corner). The cell fills its grid column (2 columns), so measure
  // its width and scale the fixed 1280×800 viewport to fit.
  const VIEW_W = 1280
  const VIEW_H = 800
  const cellRef = useRef<HTMLDivElement>(null)
  const [cellW, setCellW] = useState(0)
  useEffect(() => {
    if (!tiled) return
    const el = cellRef.current
    if (!el) return
    const ro = new ResizeObserver((entries) => setCellW(entries[0].contentRect.width))
    ro.observe(el)
    return () => ro.disconnect()
  }, [tiled])
  const scale = cellW > 0 ? cellW / VIEW_W : 0.34
  const scan = useScanStore((st) => (tiled ? st.scanTabs[tab.id] : undefined))

  // The webview stays inside the SAME wrapper div whether tiled or not — only
  // styles toggle. Changing the root element (bare <webview> ↔ wrapped) would
  // remount the webview and reload it to src=initialUrl, snapping the page back
  // to the default URL when a scan starts.
  return (
    <div
      ref={cellRef}
      title={tiled ? tab.title || tab.url : undefined}
      style={
        (tiled
          ? {
              width: '100%',
              height: VIEW_H * scale,
              overflow: 'hidden',
              position: 'relative',
              borderRadius: 4,
              border: active ? '2px solid var(--accent)' : '1px solid var(--border)',
              background: 'var(--viewport-bg)'
            }
          : {
              flex: 1,
              minWidth: 0,
              display: active ? 'flex' : 'none'
            }) as React.CSSProperties
      }
    >
      <webview
        ref={wvRef as unknown as React.Ref<HTMLElement>}
        src={tab.initialUrl}
        style={
          (tiled
            ? {
                width: VIEW_W,
                height: VIEW_H,
                transform: `scale(${scale})`,
                transformOrigin: 'top left',
                background: 'var(--viewport-bg)',
                display: 'flex'
              }
            : {
                flex: 1,
                minWidth: 0,
                background: 'var(--viewport-bg)',
                display: 'flex'
              }) as React.CSSProperties
        }
        allowpopups={'true' as unknown as boolean}
        // Shared partition → all tabs share one cookie/storage jar like a normal
        // browser profile (must match SHARED_PARTITION in main/tab-partition.ts).
        // Proxy tabs carry their own in-memory partition (incognito-style).
        partition={tab.partition ?? 'persist:rever-shared'}
        // The tab has its own webContents, so the window's backgroundThrottling
        // setting does not reach it. Without this, CDP input dispatch into a
        // backgrounded window is acked only every ~5s.
        webpreferences="backgroundThrottling=false"
      />
      {tiled && (
        <div
          style={{
            position: 'absolute',
            left: 0,
            right: 0,
            bottom: 0,
            padding: '2px 6px',
            fontSize: 10,
            color: 'var(--text-2)',
            background: 'var(--bg-bar)',
            borderTop: '1px solid var(--border)',
            display: 'flex',
            alignItems: 'center',
            gap: 6
          }}
        >
          {scan && (
            <span
              style={{
                flexShrink: 0,
                fontWeight: 600,
                color:
                  scan.status === 'done'
                    ? 'var(--status-ok, #5fd07a)'
                    : scan.status === 'error'
                      ? 'var(--status-error)'
                      : 'var(--chip-info-text)'
              }}
            >
              {scan.status === 'inspecting'
                ? '● 검사 중'
                : scan.status === 'done'
                  ? `✓ 발견 ${scan.findings}`
                  : scan.status === 'error'
                    ? '✕ 오류'
                    : '… 로딩'}
            </span>
          )}
          <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
            {tab.title || tab.url}
          </span>
        </div>
      )}
    </div>
  )
})
