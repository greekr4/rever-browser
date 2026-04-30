import { contextBridge, ipcRenderer } from 'electron'

export interface AcpAgentDef {
  id: string
  command: string
  args: string[]
}

export interface AcpSessionUpdate {
  sessionId: string
  update: Record<string, unknown>
}

export type ViewportMode = 'desktop' | 'mobile'

export type AiActionKind =
  | 'navigate'
  | 'click'
  | 'type'
  | 'scroll'
  | 'snapshot'
  | 'screenshot'
  | 'evaluate'

export interface AiAction {
  kind: AiActionKind
  label: string
  detail?: string
  ts: number
}

export interface StoredRequestSummary {
  requestId: string
  url: string
  host: string
  method: string
  resourceType: string
  startedAt: number
  completedAt?: number
  status?: number
  mimeType?: string
  encodedDataLength?: number
  requestHeaders?: Record<string, string>
  requestPostData?: string
  responseHeaders?: Record<string, string>
  responseBody?: string
  responseBodyBase64?: boolean
  responseBodyError?: string
}

const api = {
  cdp: {
    attach: (webContentsId: number) => ipcRenderer.invoke('cdp:attach', webContentsId),
    detach: (webContentsId: number) => ipcRenderer.invoke('cdp:detach', webContentsId),
    setActive: (webContentsId: number) => ipcRenderer.invoke('cdp:set-active', webContentsId),
    onNewWindow: (
      handler: (payload: { url: string; sourceWebContentsId: number }) => void
    ): (() => void) => {
      const listener = (
        _e: unknown,
        payload: { url: string; sourceWebContentsId: number }
      ) => handler(payload)
      ipcRenderer.on('webview:new-window', listener)
      return () => ipcRenderer.removeListener('webview:new-window', listener)
    }
  },
  onNetworkEvent: (handler: (payload: unknown) => void) => {
    const listener = (_e: unknown, payload: unknown) => handler(payload)
    ipcRenderer.on('network-event', listener)
    return () => ipcRenderer.removeListener('network-event', listener)
  },
  acp: {
    spawn: (agentDef: AcpAgentDef, cwd: string): Promise<{ sessionId: string }> =>
      ipcRenderer.invoke('acp:spawn', agentDef, cwd),

    prompt: (
      sessionId: string,
      text: string,
      onUpdate: (notification: AcpSessionUpdate) => void
    ): Promise<{ stopReason: string }> => {
      const channel = `acp:update:${sessionId}:${Date.now()}:${Math.random().toString(36).slice(2, 8)}`
      const listener = (_e: unknown, notification: AcpSessionUpdate) => onUpdate(notification)
      ipcRenderer.on(channel, listener)
      return ipcRenderer
        .invoke('acp:prompt', sessionId, text, channel)
        .finally(() => ipcRenderer.removeListener(channel, listener))
    },

    cancel: (sessionId: string): Promise<void> => ipcRenderer.invoke('acp:cancel', sessionId),
    kill: (sessionId: string): Promise<void> => ipcRenderer.invoke('acp:kill', sessionId)
  },
  viewport: {
    get: (): Promise<ViewportMode> => ipcRenderer.invoke('viewport:get'),
    set: (mode: ViewportMode): Promise<ViewportMode> => ipcRenderer.invoke('viewport:set', mode),
    onChange: (handler: (mode: ViewportMode) => void): (() => void) => {
      const listener = (_e: unknown, mode: ViewportMode) => handler(mode)
      ipcRenderer.on('viewport-changed', listener)
      return () => {
        ipcRenderer.removeListener('viewport-changed', listener)
      }
    }
  },
  traffic: {
    get: (requestId: string): Promise<StoredRequestSummary | null> =>
      ipcRenderer.invoke('traffic:get', requestId)
  },
  aiAction: {
    subscribe: (handler: (action: AiAction) => void): (() => void) => {
      const listener = (_e: unknown, action: AiAction) => handler(action)
      ipcRenderer.on('ai:action', listener)
      return () => ipcRenderer.removeListener('ai:action', listener)
    }
  },
  onReloadRequest: (handler: (opts: { ignoreCache: boolean }) => void): (() => void) => {
    const listener = (_e: unknown, opts: { ignoreCache: boolean }) => handler(opts)
    ipcRenderer.on('reload-webview', listener)
    return () => {
      ipcRenderer.removeListener('reload-webview', listener)
    }
  }
}

export type RevAPI = typeof api

contextBridge.exposeInMainWorld('rev', api)
