import { forwardRef, useEffect, useImperativeHandle, useRef } from 'react'

import type { Tab } from '@/stores/tabs'
import { useTabsStore } from '@/stores/tabs'

interface Props {
  tab: Tab
  active: boolean
}

export interface WebviewTabHandle {
  loadURL: (url: string) => void
  goBack: () => void
  goForward: () => void
  reload: (ignoreCache?: boolean) => void
}

interface PageTitleEvent extends Event {
  title: string
}

export const WebviewTab = forwardRef<WebviewTabHandle, Props>(function WebviewTab(
  { tab, active },
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
          console.log('[webview] deferring loadURL until dom-ready', url)
          const onReady = () => {
            wv.removeEventListener('dom-ready', onReady)
            console.log('[webview] dom-ready, loading deferred URL', url)
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
      }
    }),
    []
  )

  useEffect(() => {
    const wv = wvRef.current
    if (!wv) return
    let attached = false

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
        updateTab(tab.id, { webContentsId: id })
      }
    }

    const onNavigate = (e: Electron.DidNavigateEvent) => {
      updateTab(tab.id, { url: e.url })
    }
    const onNavigateInPage = (e: Electron.DidNavigateInPageEvent) => {
      updateTab(tab.id, { url: e.url })
    }
    const onTitle = (e: Event) => {
      const ev = e as PageTitleEvent
      if (ev.title) updateTab(tab.id, { title: ev.title })
    }

    wv.addEventListener('dom-ready', tryAttach)
    wv.addEventListener('did-attach', tryAttach)
    wv.addEventListener('did-finish-load', tryAttach)
    wv.addEventListener('did-navigate', onNavigate)
    wv.addEventListener('did-navigate-in-page', onNavigateInPage)
    wv.addEventListener('page-title-updated', onTitle)

    return () => {
      wv.removeEventListener('dom-ready', tryAttach)
      wv.removeEventListener('did-attach', tryAttach)
      wv.removeEventListener('did-finish-load', tryAttach)
      wv.removeEventListener('did-navigate', onNavigate)
      wv.removeEventListener('did-navigate-in-page', onNavigateInPage)
      wv.removeEventListener('page-title-updated', onTitle)

      // Best-effort detach when the tab unmounts (closed).
      const id = tab.webContentsId
      if (id) void window.rev.cdp.detach(id)
    }
    // tab.id is stable for a tab's lifetime; don't depend on tab.url which
    // would tear down listeners on every navigation.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab.id])

  return (
    <webview
      ref={wvRef as unknown as React.Ref<HTMLElement>}
      src={tab.initialUrl}
      style={
        {
          flex: 1,
          minWidth: 0,
          background: '#000',
          display: active ? 'flex' : 'none'
        } as React.CSSProperties
      }
      allowpopups={'true' as unknown as boolean}
      partition="persist:rever"
    />
  )
})
