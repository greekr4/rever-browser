import { webContents, type WebContents, type Debugger } from 'electron'

import { VISUALIZER_INIT_SCRIPT } from './mcp/visualizer'
import { getRequest, upsertRequest } from './traffic-store'

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

export function setActiveTarget(id: number): boolean {
  if (!attached.has(id)) return false
  activeWebContentsId = id
  return true
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
}

interface ResponseReceivedParams {
  requestId: string
  response: {
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

// Pick WebGL vendor/renderer matching the actual host hardware so OS-GPU
// cross-checks (Google CAPTCHA, etc.) don't trigger on a mismatch.
function pickWebGLIdentity(): { vendor: string; renderer: string } {
  if (process.platform === 'darwin') {
    if (process.arch === 'arm64') {
      // Apple Silicon — match what real Chrome reports
      return {
        vendor: 'Google Inc. (Apple)',
        renderer: 'ANGLE (Apple, ANGLE Metal Renderer: Apple M2, Unspecified Version)'
      }
    }
    return { vendor: 'Intel Inc.', renderer: 'Intel Iris OpenGL Engine' }
  }
  if (process.platform === 'win32') {
    return {
      vendor: 'Google Inc. (Intel)',
      renderer: 'ANGLE (Intel, Intel(R) UHD Graphics Direct3D11 vs_5_0 ps_5_0, D3D11)'
    }
  }
  return { vendor: 'Mesa/X.org', renderer: 'Mesa Intel(R) UHD Graphics (TGL GT2)' }
}

// Bot-detection bypass injected before every document loads in the webview.
// Patches the standard signals that WAFs (yes24 Code 12, Cloudflare, Akamai, PerimeterX) check:
// navigator.webdriver, chrome.runtime, plugins, languages, permissions, hardwareConcurrency,
// outerWidth/Height, WebGL vendor/renderer.
const WEBGL_IDENTITY = pickWebGLIdentity()
const STEALTH_INIT_SCRIPT = `
(() => {
  try {
    // 1. navigator.webdriver — redefine on Navigator.prototype (CDP attach sets this true on prototype)
    try {
      Object.defineProperty(Navigator.prototype, 'webdriver', {
        get: () => false,
        configurable: true,
        enumerable: false
      })
    } catch {}
    try { delete navigator.webdriver } catch {}

    // 2. chrome.runtime
    if (!window.chrome) window.chrome = {}
    if (!window.chrome.runtime) {
      window.chrome.runtime = {
        PlatformOs: { MAC: 'mac', WIN: 'win', ANDROID: 'android', CROS: 'cros', LINUX: 'linux', OPENBSD: 'openbsd' },
        PlatformArch: { ARM: 'arm', X86_32: 'x86-32', X86_64: 'x86-64' }
      }
    }

    // 3. navigator.plugins — must be a real PluginArray (instanceof check), not a plain Array
    try {
      const makePlugin = (name, filename, description) => {
        const mime = Object.create(MimeType.prototype)
        Object.defineProperties(mime, {
          type: { value: 'application/pdf' },
          suffixes: { value: 'pdf' },
          description: { value: description }
        })
        const plugin = Object.create(Plugin.prototype)
        Object.defineProperties(plugin, {
          name: { value: name },
          filename: { value: filename },
          description: { value: description },
          length: { value: 1 },
          0: { value: mime },
          item: { value: () => mime },
          namedItem: { value: () => mime }
        })
        mime.enabledPlugin = plugin
        return plugin
      }
      const p1 = makePlugin('PDF Viewer', 'internal-pdf-viewer', 'Portable Document Format')
      const p2 = makePlugin('Chrome PDF Viewer', 'internal-pdf-viewer', 'Portable Document Format')
      const p3 = makePlugin('Chromium PDF Viewer', 'internal-pdf-viewer', 'Portable Document Format')
      const fakePlugins = Object.create(PluginArray.prototype)
      Object.defineProperties(fakePlugins, {
        length: { value: 3 },
        0: { value: p1, enumerable: true },
        1: { value: p2, enumerable: true },
        2: { value: p3, enumerable: true },
        item: { value: (i) => fakePlugins[i] || null },
        namedItem: { value: (n) => [p1, p2, p3].find((p) => p.name === n) || null },
        refresh: { value: () => {} }
      })
      Object.defineProperty(Navigator.prototype, 'plugins', {
        get: () => fakePlugins,
        configurable: true
      })
    } catch {}

    // 4. navigator.languages — must align with Accept-Language
    Object.defineProperty(navigator, 'languages', {
      get: () => ['ko-KR', 'ko', 'en-US', 'en'],
      configurable: true
    })

    // 5. permissions API consistency (notifications)
    const origQuery = navigator.permissions && navigator.permissions.query
    if (origQuery) {
      navigator.permissions.query = (params) =>
        params && params.name === 'notifications'
          ? Promise.resolve({ state: Notification.permission })
          : origQuery.call(navigator.permissions, params)
    }

    // 6. hardware
    Object.defineProperty(navigator, 'hardwareConcurrency', { get: () => 8, configurable: true })
    Object.defineProperty(navigator, 'deviceMemory', { get: () => 8, configurable: true })

    // 7. outer dimensions offset (headless has them == inner)
    Object.defineProperty(window, 'outerWidth', { get: () => window.innerWidth, configurable: true })
    Object.defineProperty(window, 'outerHeight', { get: () => window.innerHeight + 85, configurable: true })

    // 8. WebGL vendor/renderer — match host hardware so OS-GPU cross-checks pass
    const __vendor = ${JSON.stringify(WEBGL_IDENTITY.vendor)}
    const __renderer = ${JSON.stringify(WEBGL_IDENTITY.renderer)}
    const getParam = WebGLRenderingContext.prototype.getParameter
    WebGLRenderingContext.prototype.getParameter = function (p) {
      if (p === 37445) return __vendor
      if (p === 37446) return __renderer
      return getParam.apply(this, arguments)
    }
    if (window.WebGL2RenderingContext) {
      const getParam2 = WebGL2RenderingContext.prototype.getParameter
      WebGL2RenderingContext.prototype.getParameter = function (p) {
        if (p === 37445) return __vendor
        if (p === 37446) return __renderer
        return getParam2.apply(this, arguments)
      }
    }
  } catch (e) {
    // Never break the page; stealth is best-effort
    console && console.debug && console.debug('[stealth]', e)
  }
})();
`

const SKIP_BODY_PREFIXES = ['image/', 'video/', 'audio/', 'font/']
const SKIP_BODY_TYPES = new Set(['Image', 'Media', 'Font', 'Stylesheet'])

function shouldFetchBody(mimeType: string | undefined, resourceType: string): boolean {
  if (SKIP_BODY_TYPES.has(resourceType)) return false
  if (mimeType && SKIP_BODY_PREFIXES.some((p) => mimeType.startsWith(p))) return false
  return true
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

  // Intercept window.open / target=_blank on this webview and ask the
  // renderer to open a new tab inside the app instead of a new OS window.
  target.setWindowOpenHandler(({ url }) => {
    sink.send('webview:new-window', { url, sourceWebContentsId: targetId })
    return { action: 'deny' }
  })

  void dbg.sendCommand('Network.enable').catch((e) => console.error('[cdp] Network.enable:', e))
  void dbg
    .sendCommand('Network.setExtraHTTPHeaders', {
      headers: { 'Accept-Language': 'ko-KR,ko;q=0.9,en-US;q=0.8,en;q=0.7' }
    })
    .catch((e) => console.error('[cdp] setExtraHTTPHeaders:', e))
  void dbg
    .sendCommand('Page.enable')
    .then(async () => {
      await dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', {
        source: STEALTH_INIT_SCRIPT
      })
      await dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', {
        source: VISUALIZER_INIT_SCRIPT
      })
      // Inject visualizer into the already-loaded document too, so it's
      // available immediately on the very first page (before any navigation).
      await dbg
        .sendCommand('Runtime.evaluate', { expression: VISUALIZER_INIT_SCRIPT })
        .catch(() => {})
    })
    .catch((e) => console.error('[cdp] init scripts:', e))

  dbg.on('message', (_event, method, params) => {
    if (method === 'Network.requestWillBeSent') {
      const p = params as RequestWillBeSentParams
      bumpInFlight(targetId, +1)
      const resourceType = p.type ?? 'Other'
      upsertRequest({
        requestId: p.requestId,
        url: p.request.url,
        host: hostFromUrl(p.request.url),
        method: p.request.method,
        resourceType,
        startedAt: Date.now(),
        requestHeaders: p.request.headers,
        requestPostData: p.request.postData
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
      upsertRequest({
        requestId: p.requestId,
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
          const res = (await dbg.sendCommand('Network.getResponseBody', {
            requestId: p.requestId
          })) as { body: string; base64Encoded: boolean }
          upsertRequest({
            requestId: p.requestId,
            responseBody: res.body,
            responseBodyBase64: res.base64Encoded
          })
        } catch (e) {
          upsertRequest({
            requestId: p.requestId,
            responseBodyError: e instanceof Error ? e.message : String(e)
          })
        }
      })()
    }
  })

  dbg.on('detach', (_event, reason) => {
    console.warn('[cdp] detached from', targetId, reason)
    attached.delete(targetId)
    inFlight.delete(targetId)
  })

  attached.set(targetId, { dbg, wc: target })
  return true
}

export function detachCdpCapture(targetId: number): boolean {
  const t = attached.get(targetId)
  if (!t) return false
  try {
    t.dbg.detach()
  } catch {}
  attached.delete(targetId)
  if (activeWebContentsId === targetId) activeWebContentsId = null
  return true
}
