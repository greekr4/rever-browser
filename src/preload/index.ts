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

export type AgentHealthStatus =
  | 'ready'
  | 'not-installed'
  | 'needs-key'
  | 'needs-login'
  | 'auth-failed'
  | 'failed'

export interface AgentProbeDef {
  id: string
  provider?: 'acp' | 'anthropic' | 'openai'
  command: string
  fallbackBins?: string[]
}

export interface AgentHealth {
  id: string
  status: AgentHealthStatus
  resolvedPath: string | null
  detail: string | null
  auto: boolean
  plan: string | null
}

export interface AcpSessionUpdate {
  sessionId: string
  update: Record<string, unknown>
}

export type ViewportMode = 'desktop' | 'mobile'

export type AiActionKind =
  | 'navigate'
  | 'click'
  | 'hover'
  | 'type'
  | 'scroll'
  | 'snapshot'
  | 'screenshot'
  | 'evaluate'
  | 'extract'
  | 'analyze'

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

// Mirrors main/tab-partition.ts BrowserProfile — keep the two in sync.
export type ProfileKind = 'persistent' | 'incognito'
export interface BrowserProfile {
  id: string
  name: string
  kind: ProfileKind
  partition: string
  source?: string
}

// Mirrors main/browser-cookie-import.ts — keep the two in sync.
export type BrowserId =
  | 'chrome'
  | 'edge'
  | 'brave'
  | 'arc'
  | 'chromium'
  | 'vivaldi'
  | 'firefox'
  | 'safari'
export interface BrowserProfileInfo {
  id: string
  name: string
}
export interface BrowserInfo {
  id: BrowserId
  name: string
  profiles: BrowserProfileInfo[]
}

export interface GrabCapture {
  selector: string | null
  ref: string | null
  rect: { x: number; y: number; width: number; height: number } | null
  info: { tag: string | null; text: string; id: string | null; cls: string | null } | null
  // PNG data URL of the grabbed element (or full viewport when no box model).
  dataUrl: string | null
}

export interface McpToolInfo {
  name: string
  description?: string
  inputSchema: unknown
}

export interface WorkflowRunStep {
  tool: string
  input: Record<string, unknown>
  // Preconditions, both applied BEFORE the tool runs: wait until `waitFor`
  // matches an element, then pause a further `delay` ms.
  waitFor?: string
  delay?: number
  // Cap for waitFor polling, ms (default 10000).
  waitTimeout?: number
}

export interface WorkflowStepProgress {
  index: number
  tool: string
  status: 'waiting' | 'running' | 'done' | 'error'
  output?: string
  error?: string
  // Human-readable reason shown while status is 'waiting'.
  waitingFor?: string
}

export interface PipeCond {
  on: 'output' | 'error'
  op: 'contains' | 'equals' | 'matches' | 'always'
  value: string
}

export type ResolvedPipeNode =
  | { id: string; type: 'tool'; tool: string; input: Record<string, unknown> }
  | { id: string; type: 'if'; cond: PipeCond; then: ResolvedPipeNode[]; else: ResolvedPipeNode[] }

export interface PipeProgress {
  nodeId: string
  tool?: string
  status: 'running' | 'done' | 'error' | 'branch'
  output?: string
  error?: string
  taken?: 'then' | 'else'
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
      handler: (payload: { url: string; disposition: string; sourceWebContentsId: number }) => void
    ): (() => void) => {
      const listener = (
        _e: unknown,
        payload: { url: string; disposition: string; sourceWebContentsId: number }
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
  onElementCopied: (
    handler: (payload: {
      rect: { x: number; y: number; width: number; height: number } | null
      selector: string | null
      ref: string | null
    }) => void
  ): (() => void) => {
    const listener = (
      _e: unknown,
      payload: {
        rect: { x: number; y: number; width: number; height: number } | null
        selector: string | null
        ref: string | null
      }
    ) => handler(payload)
    ipcRenderer.on('element-copied', listener)
    return () => ipcRenderer.removeListener('element-copied', listener)
  },
  picker: {
    start: (): Promise<void> => ipcRenderer.invoke('picker:start'),
    stop: (): Promise<void> => ipcRenderer.invoke('picker:stop'),
    onState: (
      handler: (payload: { active: boolean; mode: 'pick' | 'grab' | null }) => void
    ): (() => void) => {
      const listener = (_e: unknown, payload: { active: boolean; mode: 'pick' | 'grab' | null }) =>
        handler(payload)
      ipcRenderer.on('picker:state', listener)
      return () => ipcRenderer.removeListener('picker:state', listener)
    }
  },
  // The /rever Claude Code skill: check whether it's installed (the install
  // command is shown for the user to run themselves).
  skill: {
    status: (): Promise<{ installed: boolean }> => ipcRenderer.invoke('skill:status')
  },
  // Local CLI agent running in a PTY (terminal mode), wired to rever's MCP.
  terminal: {
    spawn: (opts: { cols: number; rows: number; agent: 'claude' | 'shell' }): Promise<string> =>
      ipcRenderer.invoke('terminal:spawn', opts),
    write: (id: string, data: string): void => ipcRenderer.send('terminal:input', id, data),
    resize: (id: string, cols: number, rows: number): void =>
      ipcRenderer.send('terminal:resize', id, cols, rows),
    kill: (id: string): void => ipcRenderer.send('terminal:kill', id),
    onData: (id: string, handler: (data: string) => void): (() => void) => {
      const ch = `terminal:data:${id}`
      const listener = (_e: unknown, data: string) => handler(data)
      ipcRenderer.on(ch, listener)
      return () => ipcRenderer.removeListener(ch, listener)
    },
    onExit: (id: string, handler: (code: number) => void): (() => void) => {
      const ch = `terminal:exit:${id}`
      const listener = (_e: unknown, code: number) => handler(code)
      ipcRenderer.on(ch, listener)
      return () => ipcRenderer.removeListener(ch, listener)
    }
  },
  // Grab: pick an element to capture its screenshot + context for the agent.
  // Shares the picker's inspect overlay, so picker:state also reflects grab mode.
  grab: {
    start: (): Promise<void> => ipcRenderer.invoke('grab:start'),
    stop: (): Promise<void> => ipcRenderer.invoke('grab:stop'),
    onCaptured: (handler: (payload: GrabCapture) => void): (() => void) => {
      const listener = (_e: unknown, payload: GrabCapture) => handler(payload)
      ipcRenderer.on('grab:captured', listener)
      return () => ipcRenderer.removeListener('grab:captured', listener)
    }
  },
  acp: {
    listAvailable: (probes: AcpAgentProbe[]): Promise<AcpAgentProbeResult[]> =>
      ipcRenderer.invoke('acp:list-available', probes),

    // Live health check: runs the ACP handshake / an authenticated API call so
    // a broken install or an expired key doesn't show up as "ready".
    probe: (defs: AgentProbeDef[]): Promise<AgentHealth[]> =>
      ipcRenderer.invoke('agent:probe', defs),

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
      ipcRenderer.invoke('theme:set-titlebar', resolved),
    // Force `prefers-color-scheme` inside one webview (null clears it) so the
    // loaded site renders its own light/dark stylesheet instead of the OS's.
    setWebviewScheme: (
      webContentsId: number,
      scheme: 'light' | 'dark' | null
    ): Promise<boolean> => ipcRenderer.invoke('theme:set-webview-scheme', webContentsId, scheme)
  },
  proxy: {
    // Apply (or clear, with null) the given tab's upstream proxy. Pass
    // `partition` once when creating an isolated proxy tab — main registers
    // the tab→partition mapping before applying the proxy.
    set: (tabId: string, config: ProxyConfig | null, partition?: string): Promise<boolean> =>
      ipcRenderer.invoke('proxy:set', tabId, config, partition),
    // Tell main which tab is active so cookie import / sticky-cookie snapshot
    // target the right partition.
    setActiveTab: (tabId: string): Promise<boolean> =>
      ipcRenderer.invoke('tab:set-active-partition', tabId)
  },
  // Named browsing profiles. Persistent profiles survive restarts and back
  // cookie import; incognito profiles are ephemeral (in-memory partition).
  profiles: {
    list: (): Promise<BrowserProfile[]> => ipcRenderer.invoke('profiles:list'),
    create: (name: string, kind: ProfileKind, source?: string): Promise<BrowserProfile> =>
      ipcRenderer.invoke('profiles:create', name, kind, source),
    delete: (id: string): Promise<boolean> => ipcRenderer.invoke('profiles:delete', id),
    // Register a profile tab's partition with main (no proxy) so cookie import /
    // current-ip target that profile's session.
    registerTabPartition: (tabId: string, partition: string): Promise<boolean> =>
      ipcRenderer.invoke('tab:register-partition', tabId, partition)
  },
  // Current outbound public IP as seen through the given tab's session (so a
  // tab-level proxy is reflected). Falls back to the active tab's partition
  // when tabId is omitted. Never rejects — errors come back as { error }.
  getCurrentIp: (tabId?: string): Promise<{ ip?: string; error?: string }> =>
    ipcRenderer.invoke('net:current-ip', tabId),
  workflows: {
    // Available MCP tools, for the macro step editor.
    listTools: (): Promise<McpToolInfo[]> => ipcRenderer.invoke('workflow:list-tools'),
    // Run a resolved macro (steps already have {{var}} substituted). Progress
    // is streamed per step to onProgress.
    run: (
      steps: WorkflowRunStep[],
      onProgress: (p: WorkflowStepProgress) => void
    ): Promise<WorkflowStepProgress[]> => {
      const channel = `workflow:progress:${Date.now()}:${Math.random().toString(36).slice(2, 8)}`
      const listener = (_e: unknown, p: WorkflowStepProgress): void => onProgress(p)
      ipcRenderer.on(channel, listener)
      return ipcRenderer
        .invoke('workflow:run', steps, channel)
        .finally(() => ipcRenderer.removeListener(channel, listener))
    },
    // Run a resolved pipeline (branch-aware node tree).
    runPipeline: (
      nodes: ResolvedPipeNode[],
      onProgress: (p: PipeProgress) => void
    ): Promise<boolean> => {
      const channel = `workflow:pipe:${Date.now()}:${Math.random().toString(36).slice(2, 8)}`
      const listener = (_e: unknown, p: PipeProgress): void => onProgress(p)
      ipcRenderer.on(channel, listener)
      return ipcRenderer
        .invoke('workflow:run-pipeline', nodes, channel)
        .finally(() => ipcRenderer.removeListener(channel, listener))
    },
    cancel: (): Promise<boolean> => ipcRenderer.invoke('workflow:cancel')
  },
  bridge: {
    // Answer main-process requests (MCP tools reaching renderer-owned state).
    // The handler's resolved value is sent back on the matching id.
    onRequest: (handler: (op: string, payload: unknown) => Promise<unknown>): (() => void) => {
      const listener = (_e: unknown, msg: { id: number; op: string; payload: unknown }): void => {
        Promise.resolve(handler(msg.op, msg.payload))
          .then((result) => ipcRenderer.send('bridge:response', { id: msg.id, ok: true, result }))
          .catch((e: unknown) =>
            ipcRenderer.send('bridge:response', {
              id: msg.id,
              ok: false,
              error: e instanceof Error ? e.message : String(e)
            })
          )
      }
      ipcRenderer.on('bridge:request', listener)
      return () => {
        ipcRenderer.removeListener('bridge:request', listener)
      }
    }
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
    // Detected browsers (installed with importable profiles).
    browsers: (): Promise<BrowserInfo[]> => ipcRenderer.invoke('browser-cookies:list'),
    browserImport: (opts: {
      browser: BrowserId
      profile?: string
      hosts?: string[]
    }): Promise<{
      ok: boolean
      imported: number
      skipped: number
      undecryptable: number
      total: number
      error?: string
    }> => ipcRenderer.invoke('browser-cookies:import', opts)
  },
  onReloadRequest: (handler: (opts: { ignoreCache: boolean }) => void): (() => void) => {
    const listener = (_e: unknown, opts: { ignoreCache: boolean }) => handler(opts)
    ipcRenderer.on('reload-webview', listener)
    return () => {
      ipcRenderer.removeListener('reload-webview', listener)
    }
  },
  // Browser-level shortcuts (Cmd/Ctrl+T, +W, tab switching, ...) forwarded
  // from main's menu accelerators / before-input-event interceptors.
  onBrowserCommand: (
    handler: (opts: {
      cmd:
        | 'new-tab'
        | 'close-tab'
        | 'reopen-tab'
        | 'next-tab'
        | 'prev-tab'
        | 'select-tab'
        | 'back'
        | 'forward'
        | 'focus-address'
        | 'find'
        | 'open-tab'
      index?: number
      url?: string
    }) => void
  ): (() => void) => {
    const listener = (_e: unknown, opts: Parameters<typeof handler>[0]) => handler(opts)
    ipcRenderer.on('browser-command', listener)
    return () => {
      ipcRenderer.removeListener('browser-command', listener)
    }
  },
  // Cmd/Ctrl+W on the last remaining tab closes the window, Chromium-style.
  closeWindow: (): void => {
    ipcRenderer.send('window:close')
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
