import { webContents, app, type WebContents, type Debugger } from 'electron'
import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

import { VISUALIZER_INIT_SCRIPT } from './mcp/visualizer'
import { clearOopifs, registerOopif, unregisterOopif } from './mcp/oopif'
import {
  getRequest,
  upsertRequest,
  appendWsFrame,
  appendConsole,
  appendException,
  type StoredRequest
} from './traffic-store'
import { STEALTH_INIT_SCRIPT, SPOOFED_CHROME_VERSION, SPOOFED_CHROME_MAJOR } from './stealth-init'
import { charsetFromContentType, encodeRefetchedBody } from './mcp/wasm-analysis'
import { compileRules, findMatchingRule, type CompiledRule } from './intercept-match'

interface AttachedTarget {
  dbg: Debugger
  wc: WebContents
}

const attached = new Map<number, AttachedTarget>()
let activeWebContentsId: number | null = null

// In-flight network request count per webContentsId. Used by waitForSettle so
// auto-snapshot doesn't fire while the page is still loading.
const inFlight = new Map<number, number>()
const bumpInFlight = (id: number, d: number) =>
  inFlight.set(id, Math.max(0, (inFlight.get(id) ?? 0) + d))

export function getInFlight(id: number): number {
  return inFlight.get(id) ?? 0
}

/**
 * Wait until the active target's network has been quiet for `idleMs` or
 * `timeoutMs` elapses. Cheap alternative to Playwright's waitForLoadState —
 * relies on the in-flight counter we maintain from CDP Network events.
 */
export async function waitForSettle(
  opts: { idleMs?: number; timeoutMs?: number; minWaitMs?: number } = {}
): Promise<void> {
  const idleMs = opts.idleMs ?? 500
  const timeoutMs = opts.timeoutMs ?? 1500
  const minWaitMs = opts.minWaitMs ?? 250
  const t = getActiveTarget()
  if (!t) {
    await new Promise((r) => setTimeout(r, minWaitMs))
    return
  }
  const targetId = [...attached.entries()].find(([, v]) => v === t)?.[0]
  if (targetId == null) {
    await new Promise((r) => setTimeout(r, minWaitMs))
    return
  }

  await new Promise((r) => setTimeout(r, minWaitMs))
  const start = Date.now()
  let quietSince = inFlight.get(targetId) === 0 ? Date.now() : 0
  while (Date.now() - start < timeoutMs) {
    const n = inFlight.get(targetId) ?? 0
    if (n === 0) {
      if (quietSince === 0) quietSince = Date.now()
      if (Date.now() - quietSince >= idleMs) return
    } else {
      quietSince = 0
    }
    await new Promise((r) => setTimeout(r, 60))
  }
}

export function getActiveTarget(): AttachedTarget | null {
  if (activeWebContentsId != null) {
    const t = attached.get(activeWebContentsId)
    if (t) return t
  }
  const first = attached.values().next().value as AttachedTarget | undefined
  return first ?? null
}

/** Every attached tab, for the tab-listing tool. */
export function listTargets(): Array<{ id: number; url: string; title: string; active: boolean }> {
  const activeId = getActiveTarget() ? [...attached.entries()].find(([, v]) => v === getActiveTarget())?.[0] : null
  return [...attached.entries()].map(([id, t]) => ({
    id,
    url: t.wc.getURL(),
    title: t.wc.getTitle(),
    active: id === activeId
  }))
}

export function setActiveTarget(id: number): boolean {
  if (!attached.has(id)) return false
  activeWebContentsId = id
  return true
}

interface CallFrame {
  functionName: string
  url: string
  lineNumber: number
  columnNumber: number
}

interface Initiator {
  type: string
  url?: string
  stack?: { callFrames: CallFrame[] }
}

interface RequestWillBeSentParams {
  requestId: string
  request: {
    url: string
    method: string
    headers: Record<string, string>
    postData?: string
  }
  type?: string
  timestamp: number
  initiator?: Initiator
  redirectResponse?: object
}

interface WebSocketCreatedParams {
  requestId: string
  url: string
  timestamp: number
}

interface WebSocketFrameParams {
  requestId: string
  timestamp: number
  response: {
    opcode: number
    mask: boolean
    payloadData: string
  }
}

interface RuntimeConsoleCalledParams {
  type: string
  args: Array<{ type: string; value?: unknown; description?: string; preview?: { description?: string } }>
  timestamp: number
  stackTrace?: unknown
}

interface RuntimeExceptionThrownParams {
  timestamp: number
  exceptionDetails: {
    text: string
    exception?: unknown
    stackTrace?: unknown
  }
}

interface FetchRequestPausedParams {
  requestId: string
  request: {
    url: string
    method: string
    headers: Record<string, string>
    postData?: string
  }
  resourceType: string
  networkId?: string
  frameId?: string
  responseStatusCode?: number
  responseHeaders?: Array<{ name: string; value: string }>
}

interface DebuggerPausedParams {
  callFrames: Array<{
    callFrameId: string
    functionName: string
    location: { scriptId: string; lineNumber: number; columnNumber: number }
    scopeChain: Array<{ type: string; object: { objectId?: string; description?: string } }>
  }>
  reason: string
}

// ── Module-level state for debugger + intercept ─────────────────────────────

export interface PausedState {
  callFrames: DebuggerPausedParams['callFrames']
  reason: string
}

let debuggerPaused: PausedState | null = null

export function getDebuggerPaused(): PausedState | null {
  return debuggerPaused
}

// Intercept rules + paused queue — managed by intercept tools
export interface InterceptRule {
  id: string
  urlPattern: string
  mode: 'log' | 'block' | 'modify'
  modifyHeaders?: Record<string, string>
  replaceBody?: string
  stage: 'Request' | 'Response'
}

const interceptRules: InterceptRule[] = []
// Compiled once per setInterceptRules so the hot Fetch.requestPaused path never
// builds a RegExp from agent input (a bad pattern used to throw there and leave
// every later request paused forever).
let compiledInterceptRules: CompiledRule<InterceptRule>[] = []
const pendingFetchRequests = new Map<string, FetchRequestPausedParams>()

export function getInterceptRules(): InterceptRule[] {
  return interceptRules
}

export function getPendingFetchRequests(): Map<string, FetchRequestPausedParams> {
  return pendingFetchRequests
}

export function setInterceptRules(rules: InterceptRule[]): void {
  interceptRules.length = 0
  interceptRules.push(...rules)
  compiledInterceptRules = compileRules(interceptRules)
}

// Re-apply Fetch.enable with current rules to the active target
export async function applyFetchIntercept(): Promise<void> {
  const target = getActiveTarget()
  if (!target) return
  if (interceptRules.length === 0) {
    await target.dbg.sendCommand('Fetch.disable').catch(() => {})
    return
  }
  const patterns = interceptRules.map((r) => ({
    urlPattern: r.urlPattern,
    requestStage: r.stage
  }))
  await target.dbg
    .sendCommand('Fetch.enable', { patterns })
    .catch((e) => console.error('[cdp] Fetch.enable:', e))
}

// Breakpoints state
export interface BreakpointEntry {
  id: string
  breakpointId: string
  urlGlob: string
  line: number
  column?: number
  condition?: string
}

const breakpoints: BreakpointEntry[] = []

export function getBreakpoints(): BreakpointEntry[] {
  return breakpoints
}

export function addBreakpoint(entry: BreakpointEntry): void {
  breakpoints.push(entry)
}

export function removeBreakpoint(id: string): BreakpointEntry | undefined {
  const idx = breakpoints.findIndex((b) => b.id === id)
  if (idx === -1) return undefined
  return breakpoints.splice(idx, 1)[0]
}

// Inject snippets hook — set by inject tools
export type LoadInjectionsHook = (target: { dbg: Debugger; wc: WebContents }) => Promise<void>
let loadInjectionsHook: LoadInjectionsHook | null = null

export function setLoadInjectionsHook(fn: LoadInjectionsHook): void {
  loadInjectionsHook = fn
}

// Element-picker hook — fired when the user clicks an element while CDP
// Overlay inspect mode is active. Wired from index.ts (which owns the picker
// state, clipboard write and renderer IPC) to avoid a circular import.
type InspectNodeHandler = (backendNodeId: number) => void
let inspectNodeHandler: InspectNodeHandler | null = null

export function setInspectNodeHandler(fn: InspectNodeHandler): void {
  inspectNodeHandler = fn
}

interface ResponseReceivedParams {
  requestId: string
  type?: string
  response: {
    url: string
    status: number
    mimeType: string
    headers: Record<string, string>
  }
  timestamp: number
}

interface LoadingFinishedParams {
  requestId: string
  encodedDataLength: number
  timestamp: number
}

function hostFromUrl(url: string): string {
  try {
    return new URL(url).host
  } catch {
    return ''
  }
}

// ── JS dialog override ──────────────────────────────────────────────────────
// `alert()` / `confirm()` / `prompt()` block the renderer's main thread,
// which freezes every subsequent CDP command (including AI tool calls).
// Override them with non-blocking versions that record into a global
// history array — the page never sees a real modal and execution continues.
//
// This complements the Page.javaScriptDialogOpening handler below; the
// override script runs first (so the dialog never opens), and the CDP
// handler is the safety net for anything that slips through.
const DIALOG_OVERRIDE_SCRIPT = `
(() => {
  if (window.__revDialogOverride) return;
  window.__revDialogOverride = true;
  window.__revDialogHistory = [];
  function record(type, message, defaultValue) {
    const entry = { type, message: String(message ?? ''), ts: Date.now() };
    if (defaultValue !== undefined) entry.default = String(defaultValue);
    window.__revDialogHistory.push(entry);
    if (window.__revDialogHistory.length > 100) window.__revDialogHistory.shift();
    try { console.log('[rev-' + type + ']', message); } catch (e) {}
  }
  // The reply the next dialog gets. Without this every confirm() answered true
  // and every prompt() returned its default, so a flow gated on "cancel" or on
  // a typed value could not be driven at all. One-shot: it is consumed by the
  // first dialog, so a stale answer cannot silently apply to a later one.
  window.__revDialogAnswer = null;
  function takeAnswer() {
    const a = window.__revDialogAnswer;
    window.__revDialogAnswer = null;
    return a;
  }
  window.alert = function(msg) { record('alert', msg); takeAnswer(); };
  window.confirm = function(msg) {
    record('confirm', msg);
    const a = takeAnswer();
    return a ? a.accept !== false : true;
  };
  window.prompt = function(msg, def) {
    record('prompt', msg, def);
    const a = takeAnswer();
    if (a && a.accept === false) return null;
    if (a && typeof a.promptText === 'string') return a.promptText;
    return def == null ? '' : String(def);
  };
})();
`

// Whether to auto-dismiss any native CDP-level dialog that slips past the
// script override. Defaults true; UI/MCP toggle exposed via getter/setter.
let dialogAutoDismiss = true

export function getDialogAutoDismiss(): boolean {
  return dialogAutoDismiss
}

/**
 * Arm the answer the page's next `confirm()` / `prompt()` will receive.
 *
 * Set in the page rather than handed to `Page.handleJavaScriptDialog`: the
 * override script answers these before a native dialog ever opens, which is
 * what keeps the renderer from blocking, so the CDP handler never sees them.
 */
export async function armDialogAnswer(
  accept: boolean,
  promptText?: string
): Promise<void> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')
  await target.dbg.sendCommand('Runtime.evaluate', {
    expression: `window.__revDialogAnswer = ${JSON.stringify({ accept, promptText })}`
  })
}

export function setDialogAutoDismiss(v: boolean): void {
  dialogAutoDismiss = v
}

export interface DialogRecord {
  ts: number
  type: 'alert' | 'confirm' | 'prompt' | 'beforeunload'
  message: string
  url: string
}

const dialogHistory: DialogRecord[] = []
const MAX_DIALOG_HISTORY = 100

export function getDialogHistory(limit = 50): DialogRecord[] {
  return dialogHistory.slice(-limit)
}

export function clearDialogHistory(): void {
  dialogHistory.length = 0
}

/**
 * Force `prefers-color-scheme` inside a webview so the site renders its own
 * light/dark stylesheet instead of following the OS. `null` clears the
 * override. Requires the debugger to already be attached to `targetId`.
 */
export async function setEmulatedColorScheme(
  targetId: number,
  scheme: 'light' | 'dark' | null
): Promise<boolean> {
  const t = attached.get(targetId)
  if (!t) return false
  try {
    await t.dbg.sendCommand('Emulation.setEmulatedMedia', {
      features: scheme ? [{ name: 'prefers-color-scheme', value: scheme }] : []
    })
    return true
  } catch (e) {
    console.warn('[cdp] setEmulatedMedia failed', e)
    return false
  }
}

const SKIP_BODY_PREFIXES = ['image/', 'video/', 'audio/', 'font/']
const SKIP_BODY_TYPES = new Set(['Image', 'Media', 'Font', 'Stylesheet'])

function shouldFetchBody(mimeType: string | undefined, resourceType: string): boolean {
  if (SKIP_BODY_TYPES.has(resourceType)) return false
  if (mimeType && SKIP_BODY_PREFIXES.some((p) => mimeType.startsWith(p))) return false
  return true
}

// Cap the fallback re-fetch so a giant asset can't blow up the store.
const REFETCH_MAX_BYTES = 8 * 1024 * 1024

/**
 * Fallback body retrieval for when CDP has no copy of the response (cache hit
 * or service-worker delivery). Fetches the URL anonymously from the main
 * process. Only used for http(s) GET resources whose body we otherwise lose.
 */
async function refetchBody(
  stored: StoredRequest | undefined
): Promise<{ responseBody: string; responseBodyBase64: boolean } | null> {
  if (!stored?.url || !/^https?:/i.test(stored.url)) return null
  try {
    const res = await fetch(stored.url, { method: 'GET' })
    if (!res.ok) return null
    const buf = Buffer.from(await res.arrayBuffer())
    if (buf.length > REFETCH_MAX_BYTES) return null
    const contentType = res.headers.get('content-type') ?? undefined
    return encodeRefetchedBody(
      buf,
      contentType?.split(';')[0]?.trim(),
      charsetFromContentType(contentType)
    )
  } catch {
    return null
  }
}

export function attachCdpCapture(targetId: number, sink: WebContents): boolean {
  console.log('[cdp] attach requested for webContentsId:', targetId)
  if (attached.has(targetId)) {
    console.log('[cdp] already attached')
    return true
  }

  const target = webContents.fromId(targetId)
  if (!target) {
    console.error('[cdp] webContents.fromId returned null for id:', targetId)
    return false
  }

  let dbg: Debugger
  try {
    dbg = target.debugger
    if (dbg.isAttached()) {
      console.log('[cdp] debugger already attached, skipping')
    } else {
      dbg.attach('1.3')
    }
    console.log('[cdp] debugger attach success')
    activeWebContentsId = targetId
  } catch (e) {
    console.error('[cdp] attach failed:', e)
    return false
  }

  // Intercept window.open / target=_blank / Ctrl-Cmd-middle-click on this
  // webview and ask the renderer to open a new tab inside the app instead of a
  // new OS window. `disposition` distinguishes background-tab (Ctrl/Cmd/wheel
  // click) from foreground so we can mirror Chrome's open-in-background.
  target.setWindowOpenHandler(({ url, disposition }) => {
    sink.send('webview:new-window', { url, disposition, sourceWebContentsId: targetId })
    return { action: 'deny' }
  })

  // Cmd/Ctrl+R inside the focused webview never bubbles to the host window's
  // before-input-event, so the menu accelerator doesn't fire either. Intercept
  // here and forward to the renderer to reload the webview.
  target.on('before-input-event', (event, input) => {
    if (input.type !== 'keyDown') return
    if (input.alt) return
    if (!(input.meta || input.control)) return
    if (input.key.toLowerCase() !== 'r') return
    event.preventDefault()
    sink.send('reload-webview', { ignoreCache: input.shift })
  })

  void dbg.sendCommand('Network.enable').catch((e) => console.error('[cdp] Network.enable:', e))
  // Accept-Language is set via Network.setUserAgentOverride.acceptLanguage below.
  // Setting it here AND there caused Chromium to emit `en-US,en;q=0.9;q=0.9`
  // (a malformed duplicate q-value) which amiunique flagged at 0.02% similarity.
  // Override sec-ch-ua* client hints to match the spoofed Chrome version.
  // Without this, Chromium emits sec-ch-ua: "Chromium";v="130" (the embedded
  // version) which contradicts the UA's Chrome/148 token — pixelscan flags
  // this as masking. setUserAgentOverride with userAgentMetadata regenerates
  // every client hint header from a coherent set of values.
  const platformName = process.platform === 'darwin' ? 'macOS'
    : process.platform === 'win32' ? 'Windows'
    : 'Linux'
  const platformArch = process.arch === 'arm64' ? 'arm' : 'x86'
  const platformVersion = process.platform === 'darwin' ? '15.2.0' : '10.0.0'
  void dbg
    .sendCommand('Network.setUserAgentOverride', {
      userAgent: target.session.getUserAgent(),
      // Chromium re-applies q-weights to whatever we pass; if we pre-include
      // ';q=0.9' it gets double-formatted as `en-US,en;q=0.9;q=0.9`. Pass the
      // raw language list and let Chromium emit the proper q-weighted header.
      acceptLanguage: 'en-US,en',
      platform: platformName === 'macOS' ? 'MacIntel' : platformName === 'Windows' ? 'Win32' : 'Linux x86_64',
      userAgentMetadata: {
        brands: [
          { brand: 'Chromium', version: SPOOFED_CHROME_MAJOR },
          { brand: 'Google Chrome', version: SPOOFED_CHROME_MAJOR },
          { brand: 'Not.A/Brand', version: '99' }
        ],
        fullVersionList: [
          { brand: 'Chromium', version: SPOOFED_CHROME_VERSION },
          { brand: 'Google Chrome', version: SPOOFED_CHROME_VERSION },
          { brand: 'Not.A/Brand', version: '99.0.0.0' }
        ],
        fullVersion: SPOOFED_CHROME_VERSION,
        platform: platformName,
        platformVersion,
        architecture: platformArch,
        model: '',
        mobile: false,
        bitness: '64',
        wow64: false
      }
    })
    .catch((e) => console.error('[cdp] setUserAgentOverride:', e))
  void dbg.sendCommand('Runtime.enable').catch((e) => console.error('[cdp] Runtime.enable:', e))
  // Attach a session to every cross-site iframe so the snapshot can read it.
  // `flatten` multiplexes those sessions over this same debugger connection;
  // waitForDebuggerOnStart would stall each frame until we resumed it.
  void dbg
    .sendCommand('Target.setAutoAttach', {
      autoAttach: true,
      waitForDebuggerOnStart: false,
      flatten: true
    })
    .catch((e) => console.error('[cdp] setAutoAttach:', e))
  void dbg.sendCommand('Debugger.enable').catch((e) => console.error('[cdp] Debugger.enable:', e))
  void dbg
    .sendCommand('Page.enable')
    .then(async () => {
      await dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', {
        source: STEALTH_INIT_SCRIPT
      })
      await dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', {
        source: VISUALIZER_INIT_SCRIPT
      })
      await dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', {
        source: DIALOG_OVERRIDE_SCRIPT
      })
      // Inject stealth + visualizer + dialog override into the already-loaded
      // document too, so they're available immediately on the very first page
      // (before any navigation).
      await dbg
        .sendCommand('Runtime.evaluate', { expression: STEALTH_INIT_SCRIPT })
        .catch(() => {})
      await dbg
        .sendCommand('Runtime.evaluate', { expression: VISUALIZER_INIT_SCRIPT })
        .catch(() => {})
      await dbg
        .sendCommand('Runtime.evaluate', { expression: DIALOG_OVERRIDE_SCRIPT })
        .catch(() => {})
      // Apply user-defined injected snippets
      if (loadInjectionsHook) {
        await loadInjectionsHook({ dbg, wc: target }).catch((e) =>
          console.error('[cdp] loadInjectionsHook:', e)
        )
      }
    })
    .catch((e) => console.error('[cdp] init scripts:', e))

  // `sessionId` is set for events coming from an auto-attached out-of-process
  // frame. Reads about such an event (getResponseBody) must carry it back.
  // The root page session arrives as an empty string, which sendCommand will
  // not accept — every read must send `sessionId || undefined`.
  dbg.on('message', (_event, method, params, sessionId) => {
    if (method === 'Network.requestWillBeSent') {
      const p = params as RequestWillBeSentParams
      // 리다이렉트 체인은 같은 requestId로 requestWillBeSent를 여러 번 보낸다.
      // redirectResponse가 있으면 이전 in-flight를 이미 완료한 것이므로 net 0 (no bump).
      // redirectResponse가 없는 최초 요청만 +1 한다.
      if (!p.redirectResponse) {
        bumpInFlight(targetId, +1)
      }
      const resourceType = p.type ?? 'Other'
      const initiator = p.initiator
      upsertRequest({
        requestId: p.requestId,
        url: p.request.url,
        host: hostFromUrl(p.request.url),
        method: p.request.method,
        resourceType,
        startedAt: Date.now(),
        requestHeaders: p.request.headers,
        requestPostData: p.request.postData,
        initiatorType: initiator?.type,
        initiatorUrl: initiator?.url,
        initiatorStack: initiator?.stack?.callFrames.slice(0, 10).map((f) => ({
          functionName: f.functionName,
          url: f.url,
          lineNumber: f.lineNumber,
          columnNumber: f.columnNumber
        }))
      })
      sink.send('network-event', {
        type: 'request',
        request_id: p.requestId,
        url: p.request.url,
        method: p.request.method,
        resource_type: resourceType,
        timestamp: p.timestamp
      })
    } else if (method === 'Network.responseReceived') {
      const p = params as ResponseReceivedParams
      // Backfill url/host/resourceType here too. A cache hit (memory or disk)
      // can land responseReceived + loadingFinished for a requestId whose
      // requestWillBeSent we never saw, leaving a url-less "Other" record that
      // list_scripts and every host filter silently drop — the worst failure
      // for a reversing tool, since a cached bundle is exactly what it wants.
      // responseReceived carries the final url and the resource type, so use them.
      // p.type is omitted only when unknown; leave it off the patch in that case
      // so Object.assign can't clobber a good value from requestWillBeSent.
      upsertRequest({
        requestId: p.requestId,
        url: p.response.url,
        host: hostFromUrl(p.response.url),
        ...(p.type ? { resourceType: p.type } : {}),
        status: p.response.status,
        mimeType: p.response.mimeType,
        responseHeaders: p.response.headers
      })
      sink.send('network-event', {
        type: 'response',
        request_id: p.requestId,
        status: p.response.status,
        mime_type: p.response.mimeType,
        timestamp: p.timestamp
      })
    } else if (method === 'Network.loadingFailed') {
      bumpInFlight(targetId, -1)
    } else if (method === 'Network.loadingFinished') {
      const p = params as LoadingFinishedParams
      bumpInFlight(targetId, -1)
      upsertRequest({
        requestId: p.requestId,
        encodedDataLength: p.encodedDataLength,
        completedAt: Date.now()
      })
      sink.send('network-event', {
        type: 'finished',
        request_id: p.requestId,
        encoded_data_length: p.encodedDataLength,
        timestamp: p.timestamp
      })

      // fetch body asynchronously
      void (async () => {
        try {
          const stored = getRequest(p.requestId)
          if (!stored) return
          if (!shouldFetchBody(stored.mimeType, stored.resourceType)) return
          const res = (await dbg.sendCommand(
            'Network.getResponseBody',
            { requestId: p.requestId },
            // Electron reports the root (page) session as an empty string,
            // which sendCommand rejects — normalise it to undefined.
            sessionId || undefined
          )) as { body: string; base64Encoded: boolean }
          upsertRequest({
            requestId: p.requestId,
            responseBody: res.body,
            responseBodyBase64: res.base64Encoded
          })
        } catch (e) {
          // getResponseBody returns "No resource with given identifier found"
          // when the response was served from the memory/disk cache or by a
          // service worker — exactly how many real sites deliver their JS
          // bundles. The record would then be a url-only phantom, invisible to
          // the script tools. Re-fetch the URL from the main process as a
          // fallback: static bundles are public, so an anonymous GET returns
          // the same bytes the reverser wants to read.
          const refetched = await refetchBody(getRequest(p.requestId))
          if (refetched) {
            upsertRequest({ requestId: p.requestId, ...refetched })
          } else {
            upsertRequest({
              requestId: p.requestId,
              responseBodyError: e instanceof Error ? e.message : String(e)
            })
          }
        }
      })()
    } else if (method === 'Network.webSocketCreated') {
      const p = params as WebSocketCreatedParams
      upsertRequest({
        requestId: p.requestId,
        url: p.url ?? '',
        host: hostFromUrl(p.url ?? ''),
        method: 'GET',
        resourceType: 'WebSocket',
        startedAt: Date.now()
      })
    } else if (
      method === 'Network.webSocketWillSendHandshakeRequest' ||
      method === 'Network.webSocketHandshakeResponseReceived'
    ) {
      // Update timestamp only — URL was captured by webSocketCreated
      const p = params as { requestId: string }
      upsertRequest({ requestId: p.requestId, resourceType: 'WebSocket' })
    } else if (method === 'Network.webSocketFrameSent') {
      const p = params as WebSocketFrameParams
      appendWsFrame(p.requestId, {
        direction: 'sent',
        opcode: p.response.opcode,
        payloadData: p.response.payloadData,
        timestamp: p.timestamp,
        mask: p.response.mask
      })
    } else if (method === 'Network.webSocketFrameReceived') {
      const p = params as WebSocketFrameParams
      appendWsFrame(p.requestId, {
        direction: 'received',
        opcode: p.response.opcode,
        payloadData: p.response.payloadData,
        timestamp: p.timestamp,
        mask: p.response.mask
      })
    } else if (method === 'Runtime.consoleAPICalled') {
      const p = params as RuntimeConsoleCalledParams
      const text = p.args
        .map((a) => {
          if (a.value !== undefined) return String(a.value)
          if (a.preview?.description) return a.preview.description
          if (a.description) return a.description
          return `[${a.type}]`
        })
        .join(' ')
      // Self-probe sink: stealth script emits `[REVER_PROBE] {json}` when the
      // user lands on a fingerprint analyser site. Persist to userData so the
      // result can be inspected without re-running.
      if (p.args[0]?.value === '[REVER_PROBE]' && p.args[1]?.value) {
        try {
          const payload = JSON.parse(String(p.args[1].value))
          const dir = join(app.getPath('userData'), 'fingerprint-probes')
          mkdirSync(dir, { recursive: true })
          const safeHost = String(payload.host || 'unknown').replace(/[^a-z0-9.-]/gi, '_')
          const fname = `${safeHost}-${payload.ts || Date.now()}.json`
          writeFileSync(join(dir, fname), JSON.stringify(payload, null, 2))
          console.log('[probe] saved', fname)
        } catch (e) {
          console.warn('[probe] failed to persist', e)
        }
      }
      // CDP consoleAPICalled.timestamp는 이미 epoch 밀리초이므로 *1000 불필요
      appendConsole({ ts: Date.now(), type: p.type, text, args: p.args, stackTrace: p.stackTrace })
    } else if (method === 'Runtime.exceptionThrown') {
      const p = params as RuntimeExceptionThrownParams
      // CDP exceptionThrown.timestamp도 epoch 밀리초이므로 *1000 불필요
      appendException({
        ts: Date.now(),
        text: p.exceptionDetails.text,
        exception: p.exceptionDetails.exception,
        stackTrace: p.exceptionDetails.stackTrace
      })
    } else if (method === 'Page.javaScriptDialogOpening') {
      // Safety net for any native dialog that bypassed the script override
      // (e.g. dialogs triggered before our addScriptToEvaluateOnNewDocument
      // got a chance to run, or from `<a target=_blank>` flows in some
      // Chromium versions). Capture the content, then dismiss.
      const p = params as {
        url: string
        message: string
        type: 'alert' | 'confirm' | 'prompt' | 'beforeunload'
        defaultPrompt?: string
      }
      dialogHistory.push({
        ts: Date.now(),
        type: p.type,
        message: p.message ?? '',
        url: p.url ?? ''
      })
      if (dialogHistory.length > MAX_DIALOG_HISTORY) dialogHistory.shift()
      appendConsole({
        ts: Date.now(),
        type: 'dialog',
        text: `[${p.type}] ${p.message ?? ''}`
      })
      if (dialogAutoDismiss) {
        void dbg
          .sendCommand('Page.handleJavaScriptDialog', {
            accept: true,
            ...(p.type === 'prompt' ? { promptText: p.defaultPrompt ?? '' } : {})
          })
          .catch((e) => console.error('[cdp] handleJavaScriptDialog:', e))
      }
    } else if (method === 'Target.attachedToTarget') {
      const p = params as {
        sessionId: string
        targetInfo: { type: string; targetId: string; url: string }
      }
      // Only iframe targets matter here — workers and service workers attach
      // through the same event and have no accessibility tree to splice in.
      if (p.targetInfo.type === 'iframe') {
        registerOopif(targetId, {
          sessionId: p.sessionId,
          frameId: p.targetInfo.targetId,
          url: p.targetInfo.url,
          // The event arrives on the session that owns the <iframe>. Electron
          // reports the root session as an empty string, which sendCommand
          // will not accept as "the page" — normalise it to undefined.
          parentSessionId: sessionId || undefined
        })
        // setAutoAttach only reaches direct children, so a frame nested inside
        // this one needs the same call made on its session. A card
        // authentication step inside a payment window is exactly that shape.
        void dbg
          .sendCommand(
            'Target.setAutoAttach',
            { autoAttach: true, waitForDebuggerOnStart: false, flatten: true },
            p.sessionId
          )
          .catch(() => {})
        // Enable the domains the snapshot and click paths need, on the frame's
        // own session — enabling them on the page session does not reach it.
        void dbg.sendCommand('Accessibility.enable', {}, p.sessionId).catch(() => {})
        void dbg.sendCommand('DOM.enable', {}, p.sessionId).catch(() => {})
        // Network too, or the frame's own requests are invisible. Only the
        // iframe document load shows up without this (the parent initiates
        // that one); every call the widget makes afterwards — the payment API
        // this tool exists to read — is emitted on the frame's session alone.
        void dbg.sendCommand('Network.enable', {}, p.sessionId).catch(() => {})
      }
    } else if (method === 'Target.detachedFromTarget') {
      unregisterOopif(targetId, (params as { sessionId: string }).sessionId)
    } else if (method === 'Overlay.inspectNodeRequested') {
      inspectNodeHandler?.((params as { backendNodeId: number }).backendNodeId)
    } else if (method === 'Debugger.paused') {
      const p = params as DebuggerPausedParams
      debuggerPaused = { callFrames: p.callFrames, reason: p.reason }
    } else if (method === 'Debugger.resumed') {
      debuggerPaused = null
    } else if (method === 'Fetch.requestPaused') {
      const p = params as FetchRequestPausedParams
      // Find matching rule
      const rule = findMatchingRule(compiledInterceptRules, p.request.url)
      if (!rule) {
        // Auto-continue anything not matched
        void dbg.sendCommand('Fetch.continueRequest', { requestId: p.requestId }).catch(() => {})
        return
      }
      if (rule.mode === 'log') {
        // log mode: auto-continue, no queueing needed
        void dbg.sendCommand('Fetch.continueRequest', { requestId: p.requestId }).catch(() => {})
      } else if (rule.mode === 'block') {
        void dbg
          .sendCommand('Fetch.failRequest', {
            requestId: p.requestId,
            errorReason: 'BlockedByClient'
          })
          .catch(() => {})
      } else {
        // modify — hold in paused queue for manual intervention
        pendingFetchRequests.set(p.requestId, p)
      }
    }
  })

  dbg.on('detach', (_event, reason) => {
    console.warn('[cdp] detached from', targetId, reason)
    // 재-attach 시 핸들러 중복을 막기 위해 리스너를 제거한다.
    // removeAllListeners('detach')는 이 콜백 실행 후이므로 안전하다.
    dbg.removeAllListeners('message')
    dbg.removeAllListeners('detach')
    attached.delete(targetId)
    inFlight.delete(targetId)
    clearOopifs(targetId)
    debuggerPaused = null
    pendingFetchRequests.clear()
  })

  attached.set(targetId, { dbg, wc: target })
  return true
}

export function detachCdpCapture(targetId: number): boolean {
  const t = attached.get(targetId)
  if (!t) return false
  // 명시적 detach 시에도 리스너를 먼저 제거해 재-attach 시 중복을 방지한다.
  t.dbg.removeAllListeners('message')
  t.dbg.removeAllListeners('detach')
  try {
    t.dbg.detach()
  } catch {}
  attached.delete(targetId)
  if (activeWebContentsId === targetId) activeWebContentsId = null
  return true
}
