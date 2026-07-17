import { contextBridge, ipcRenderer } from 'electron'

import type {
  RequestPermissionRequest,
  RequestPermissionResponse
} from '@agentclientprotocol/sdk'

export interface AcpAgentDef {
  id: string
  command: string
  args: string[]
}

export interface AcpAgentProbe {
  command: string
  fallbackBins?: string[]
}

export interface AcpAgentProbeResult {
  command: string
  resolvedPath: string | null
  matchedBin: string | null
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
  | 'extract'

export interface AiAction {
  kind: AiActionKind
  label: string
  detail?: string
  ts: number
}

export interface ConsoleEntry {
  ts: number
  type: string
  text: string
  args?: unknown[]
  stackTrace?: unknown
}

export interface RuntimeException {
  ts: number
  text: string
  exception?: unknown
  stackTrace?: unknown
}

export interface WSFrame {
  direction: 'sent' | 'received'
  opcode: number
  payloadData: string
  timestamp: number
  mask?: boolean
}

export interface RepeaterRequestSpec {
  url: string
  method: string
  headers: Record<string, string>
  body?: string
}

export interface RepeaterModifications {
  url?: string
  method?: string
  setHeaders?: Record<string, string>
  removeHeaders?: string[]
  body?: string | null
}

export interface RepeaterResponse {
  status: number
  statusText: string
  headers: Record<string, string>
  body: string
  bodyTruncated: boolean
  bodyByteLength: number
  timeMs: number
  error?: string
}

export interface ProxyConfig {
  enabled: boolean
  scheme: 'http' | 'https' | 'socks5'
  host: string
  port: number
  username?: string
  password?: string
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
    listAvailable: (probes: AcpAgentProbe[]): Promise<AcpAgentProbeResult[]> =>
      ipcRenderer.invoke('acp:list-available', probes),

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
    kill: (sessionId: string): Promise<void> => ipcRenderer.invoke('acp:kill', sessionId),

    // Agent permission requests pushed from main. The handler resolves with the
    // user's decision; the result is sent back over the correlation id so the
    // agent's tool call continues. Register once (e.g. on app mount).
    onPermissionRequest: (
      handler: (request: RequestPermissionRequest) => Promise<RequestPermissionResponse>
    ): (() => void) => {
      const listener = (
        _e: unknown,
        payload: { id: string; request: RequestPermissionRequest }
      ): void => {
        Promise.resolve(handler(payload.request))
          .then((response) => ipcRenderer.send('acp:permission-respond', payload.id, response))
          .catch(() => {
            // Swallow — main's guard timeout falls back to auto-approve.
          })
      }
      ipcRenderer.on('acp:permission-request', listener)
      return () => ipcRenderer.removeListener('acp:permission-request', listener)
    },
    modelState: (
      sessionId: string
    ): Promise<{
      availableModels: Array<{ modelId: string; name: string; description?: string | null }>
      currentModelId: string | null
    } | null> => ipcRenderer.invoke('acp:model-state', sessionId),
    setModel: (sessionId: string, modelId: string): Promise<void> =>
      ipcRenderer.invoke('acp:set-model', sessionId, modelId)
  },
  theme: {
    // Keep the native window-controls overlay (Windows/Linux) in sync with the
    // app theme. No-op on macOS (traffic lights aren't recolorable this way).
    setTitlebar: (resolved: 'light' | 'dark'): Promise<void> =>
      ipcRenderer.invoke('theme:set-titlebar', resolved)
  },
  proxy: {
    // Apply (or clear, with null) the given tab's upstream proxy.
    set: (tabId: string, config: ProxyConfig | null): Promise<boolean> =>
      ipcRenderer.invoke('proxy:set', tabId, config),
    // Tell main which tab is active so cookie import / sticky-cookie snapshot
    // target the right partition.
    setActiveTab: (tabId: string): Promise<boolean> =>
      ipcRenderer.invoke('tab:set-active-partition', tabId)
  },
  settings: {
    getApiKey: (provider: 'anthropic' | 'openai'): Promise<string | null> =>
      ipcRenderer.invoke('settings:get-api-key', provider),
    hasApiKey: (provider: 'anthropic' | 'openai'): Promise<boolean> =>
      ipcRenderer.invoke('settings:has-api-key', provider),
    setApiKey: (provider: 'anthropic' | 'openai', key: string): Promise<boolean> =>
      ipcRenderer.invoke('settings:set-api-key', provider, key)
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
      ipcRenderer.invoke('traffic:get', requestId),
    clear: (): Promise<void> => ipcRenderer.invoke('traffic:clear')
  },
  repeater: {
    send: (
      requestId: string,
      modifications?: RepeaterModifications
    ): Promise<RepeaterResponse> =>
      ipcRenderer.invoke('repeater:send', requestId, modifications),
    sendRaw: (spec: RepeaterRequestSpec): Promise<RepeaterResponse> =>
      ipcRenderer.invoke('repeater:send-raw', spec)
  },
  aiAction: {
    subscribe: (handler: (action: AiAction) => void): (() => void) => {
      const listener = (_e: unknown, action: AiAction) => handler(action)
      ipcRenderer.on('ai:action', listener)
      return () => ipcRenderer.removeListener('ai:action', listener)
    }
  },
  console: {
    list: (since?: number, limit?: number): Promise<ConsoleEntry[]> =>
      ipcRenderer.invoke('console:list', since, limit),
    exceptions: (limit?: number): Promise<RuntimeException[]> =>
      ipcRenderer.invoke('console:exceptions', limit),
    clear: (): Promise<void> => ipcRenderer.invoke('console:clear')
  },
  dialog: {
    getSettings: (): Promise<{
      autoDismiss: boolean
      history: Array<{ ts: number; type: string; message: string; url: string }>
    }> => ipcRenderer.invoke('dialog:get-settings'),
    setAutoDismiss: (enabled: boolean): Promise<{ autoDismiss: boolean }> =>
      ipcRenderer.invoke('dialog:set-auto-dismiss', enabled),
    history: (limit?: number): Promise<
      Array<{ ts: number; type: string; message: string; url: string }>
    > => ipcRenderer.invoke('dialog:history', limit),
    clearHistory: (): Promise<boolean> => ipcRenderer.invoke('dialog:clear-history')
  },
  ws: {
    list: (): Promise<StoredRequestSummary[]> => ipcRenderer.invoke('ws:list'),
    frames: (requestId: string, since?: number, limit?: number): Promise<WSFrame[]> =>
      ipcRenderer.invoke('ws:frames', requestId, since, limit)
  },
  storage: {
    cookies: (urls?: string[]): Promise<{
      cookies: Array<{
        name: string
        value: string
        domain: string
        path: string
        expires?: number
        secure?: boolean
        httpOnly?: boolean
        sameSite?: string
      }>
      origin: string | null
    }> => ipcRenderer.invoke('storage:cookies', urls),
    cookieSet: (params: {
      name: string
      value: string
      url?: string
      domain?: string
      path?: string
      secure?: boolean
      httpOnly?: boolean
      sameSite?: 'Strict' | 'Lax' | 'None'
      expires?: number
    }): Promise<{ success: boolean }> => ipcRenderer.invoke('storage:cookie-set', params),
    cookieDelete: (params: {
      name: string
      url?: string
      domain?: string
      path?: string
    }): Promise<boolean> => ipcRenderer.invoke('storage:cookie-delete', params),
    localGet: (): Promise<Record<string, string>> => ipcRenderer.invoke('storage:local-get'),
    localSet: (key: string, value: string): Promise<boolean> =>
      ipcRenderer.invoke('storage:local-set', key, value),
    localDelete: (key: string): Promise<boolean> => ipcRenderer.invoke('storage:local-delete', key),
    localClear: (): Promise<boolean> => ipcRenderer.invoke('storage:local-clear'),
    sessionGet: (): Promise<Record<string, string>> => ipcRenderer.invoke('storage:session-get'),
    sessionSet: (key: string, value: string): Promise<boolean> =>
      ipcRenderer.invoke('storage:session-set', key, value),
    sessionDelete: (key: string): Promise<boolean> =>
      ipcRenderer.invoke('storage:session-delete', key),
    sessionClear: (): Promise<boolean> => ipcRenderer.invoke('storage:session-clear'),
    persistenceGet: (): Promise<{ enabled: boolean; snapshotCount: number }> =>
      ipcRenderer.invoke('cookie-persistence:get'),
    persistenceSet: (enabled: boolean): Promise<{ enabled: boolean }> =>
      ipcRenderer.invoke('cookie-persistence:set', enabled),
    persistenceSnapshot: (): Promise<{ snapshotCount: number }> =>
      ipcRenderer.invoke('cookie-persistence:snapshot'),
    chromeProfiles: (): Promise<string[]> => ipcRenderer.invoke('chrome-cookies:profiles'),
    chromeImport: (opts: {
      profile?: string
      hosts?: string[]
    }): Promise<{
      ok: boolean
      imported: number
      skipped: number
      undecryptable: number
      total: number
      error?: string
    }> => ipcRenderer.invoke('chrome-cookies:import', opts)
  },
  onReloadRequest: (handler: (opts: { ignoreCache: boolean }) => void): (() => void) => {
    const listener = (_e: unknown, opts: { ignoreCache: boolean }) => handler(opts)
    ipcRenderer.on('reload-webview', listener)
    return () => {
      ipcRenderer.removeListener('reload-webview', listener)
    }
  },
  external: {
    start: (): Promise<{ port: number; pid: number }> =>
      ipcRenderer.invoke('external:start'),
    stop: (): Promise<void> =>
      ipcRenderer.invoke('external:stop'),
    navigate: (url: string): Promise<void> =>
      ipcRenderer.invoke('external:navigate', url),
    startScreencast: (opts: {
      quality?: number
      everyNthFrame?: number
      maxWidth?: number
      maxHeight?: number
    }): Promise<void> =>
      ipcRenderer.invoke('external:start-screencast', opts),
    stopScreencast: (): Promise<void> =>
      ipcRenderer.invoke('external:stop-screencast'),
    onScreencastFrame: (
      handler: (frame: { data: string; metadata: unknown; sessionId: number }) => void
    ): (() => void) => {
      const listener = (
        _e: unknown,
        frame: { data: string; metadata: unknown; sessionId: number }
      ) => handler(frame)
      ipcRenderer.on('external:screencast-frame', listener)
      return () => ipcRenderer.removeListener('external:screencast-frame', listener)
    },
    ackFrame: (sessionId: number): Promise<void> =>
      ipcRenderer.invoke('external:ack-frame', sessionId),
    dispatchMouseEvent: (params: unknown): Promise<void> =>
      ipcRenderer.invoke('external:input-mouse', params),
    dispatchKeyEvent: (params: unknown): Promise<void> =>
      ipcRenderer.invoke('external:input-key', params)
  }
}

export type RevAPI = typeof api

contextBridge.exposeInMainWorld('rev', api)
