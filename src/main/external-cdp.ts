import CDP from 'chrome-remote-interface'
import type { WebContents } from 'electron'

import { VISUALIZER_INIT_SCRIPT } from './mcp/visualizer'
import { getRequest, upsertRequest, appendWsFrame, appendConsole, appendException } from './traffic-store'

export interface ScreencastFrameMeta {
  offsetTop: number
  pageScaleFactor: number
  deviceWidth: number
  deviceHeight: number
  scrollOffsetX: number
  scrollOffsetY: number
  timestamp?: number
}

export interface ExternalTarget {
  navigate(url: string): Promise<void>
  startScreencast(opts: {
    quality?: number
    everyNthFrame?: number
    maxWidth?: number
    maxHeight?: number
  }): Promise<void>
  stopScreencast(): Promise<void>
  ackScreencast(sessionId: number): Promise<void>
  dispatchMouseEvent(params: unknown): Promise<void>
  dispatchKeyEvent(params: unknown): Promise<void>
}

let externalClient: CDP.Client | null = null
let externalTarget: ExternalTarget | null = null

// Same stealth script as embedded mode — kept in sync with chrome-cdp.ts
// In external mode we don't need to override navigator.webdriver since real Chrome won't set it.
// We still inject the visualizer so the AI agent's flash/outline helpers work.
function hostFromUrl(url: string): string {
  try {
    return new URL(url).host
  } catch {
    return ''
  }
}

const SKIP_BODY_PREFIXES = ['image/', 'video/', 'audio/', 'font/']
const SKIP_BODY_TYPES = new Set(['Image', 'Media', 'Font', 'Stylesheet'])

function shouldFetchBody(mimeType: string | undefined, resourceType: string): boolean {
  if (SKIP_BODY_TYPES.has(resourceType)) return false
  if (mimeType && SKIP_BODY_PREFIXES.some((p) => mimeType.startsWith(p))) return false
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
  initiator?: {
    type: string
    url?: string
    stack?: {
      callFrames: Array<{
        functionName: string
        url: string
        lineNumber: number
        columnNumber: number
      }>
    }
  }
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

export async function attachExternalCdp(port: number, sink: WebContents): Promise<ExternalTarget> {
  if (externalClient) {
    // Already attached — return existing target
    if (externalTarget) return externalTarget
    try {
      await externalClient.close()
    } catch {}
    externalClient = null
  }

  // Find the first page target (not the browser-level target)
  const targets = await CDP.List({ port })
  const pageTarget = targets.find((t) => t.type === 'page') ?? targets[0]
  if (!pageTarget) throw new Error('No CDP target found in external Chrome')

  const client = await CDP({ port, target: pageTarget })
  externalClient = client

  const { Network, Page, Runtime, Debugger } = client as unknown as {
    Network: Record<string, (params?: unknown) => Promise<unknown>> & {
      requestWillBeSent: (fn: (p: RequestWillBeSentParams) => void) => void
      responseReceived: (fn: (p: ResponseReceivedParams) => void) => void
      loadingFailed: (fn: (p: { requestId: string }) => void) => void
      loadingFinished: (fn: (p: LoadingFinishedParams) => void) => void
      webSocketCreated: (fn: (p: WebSocketCreatedParams) => void) => void
      webSocketWillSendHandshakeRequest: (fn: (p: { requestId: string }) => void) => void
      webSocketHandshakeResponseReceived: (fn: (p: { requestId: string }) => void) => void
      webSocketFrameSent: (fn: (p: WebSocketFrameParams) => void) => void
      webSocketFrameReceived: (fn: (p: WebSocketFrameParams) => void) => void
    }
    Page: Record<string, (params?: unknown) => Promise<unknown>> & {
      screencastFrame: (fn: (p: { data: string; metadata: ScreencastFrameMeta; sessionId: number }) => void) => void
    }
    Runtime: Record<string, (params?: unknown) => Promise<unknown>> & {
      consoleAPICalled: (fn: (p: RuntimeConsoleCalledParams) => void) => void
      exceptionThrown: (fn: (p: RuntimeExceptionThrownParams) => void) => void
    }
    Debugger: Record<string, (params?: unknown) => Promise<unknown>>
  }

  // Enable domains
  await Network.enable()
  await Page.enable()
  await Runtime.enable()
  await Debugger.enable()

  // Inject visualizer on new documents
  await Page.addScriptToEvaluateOnNewDocument({ source: VISUALIZER_INIT_SCRIPT })
  // Inject into current document too
  await Runtime.evaluate({ expression: VISUALIZER_INIT_SCRIPT }).catch(() => {})

  // ── Network event handlers ─────────────────────────────────────────────────

  Network.requestWillBeSent((p) => {
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
    if (!sink.isDestroyed()) {
      sink.send('network-event', {
        type: 'request',
        request_id: p.requestId,
        url: p.request.url,
        method: p.request.method,
        resource_type: resourceType,
        timestamp: p.timestamp
      })
    }
  })

  Network.responseReceived((p) => {
    upsertRequest({
      requestId: p.requestId,
      status: p.response.status,
      mimeType: p.response.mimeType,
      responseHeaders: p.response.headers
    })
    if (!sink.isDestroyed()) {
      sink.send('network-event', {
        type: 'response',
        request_id: p.requestId,
        status: p.response.status,
        mime_type: p.response.mimeType,
        timestamp: p.timestamp
      })
    }
  })

  Network.loadingFailed((_p) => {
    // no-op in external mode (no inFlight counter needed here)
  })

  Network.loadingFinished((p) => {
    upsertRequest({
      requestId: p.requestId,
      encodedDataLength: p.encodedDataLength,
      completedAt: Date.now()
    })
    if (!sink.isDestroyed()) {
      sink.send('network-event', {
        type: 'finished',
        request_id: p.requestId,
        encoded_data_length: p.encodedDataLength,
        timestamp: p.timestamp
      })
    }

    // Lazy body fetch
    void (async () => {
      try {
        const stored = getRequest(p.requestId)
        if (!stored) return
        if (!shouldFetchBody(stored.mimeType, stored.resourceType)) return
        const res = (await Network.getResponseBody({ requestId: p.requestId })) as {
          body: string
          base64Encoded: boolean
        }
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
  })

  Network.webSocketCreated((p) => {
    upsertRequest({
      requestId: p.requestId,
      url: p.url ?? '',
      host: hostFromUrl(p.url ?? ''),
      method: 'GET',
      resourceType: 'WebSocket',
      startedAt: Date.now()
    })
  })

  Network.webSocketWillSendHandshakeRequest((p) => {
    upsertRequest({ requestId: p.requestId, resourceType: 'WebSocket' })
  })

  Network.webSocketHandshakeResponseReceived((p) => {
    upsertRequest({ requestId: p.requestId, resourceType: 'WebSocket' })
  })

  Network.webSocketFrameSent((p) => {
    appendWsFrame(p.requestId, {
      direction: 'sent',
      opcode: p.response.opcode,
      payloadData: p.response.payloadData,
      timestamp: p.timestamp,
      mask: p.response.mask
    })
  })

  Network.webSocketFrameReceived((p) => {
    appendWsFrame(p.requestId, {
      direction: 'received',
      opcode: p.response.opcode,
      payloadData: p.response.payloadData,
      timestamp: p.timestamp,
      mask: p.response.mask
    })
  })

  // ── Runtime events ─────────────────────────────────────────────────────────

  Runtime.consoleAPICalled((p) => {
    const text = p.args
      .map((a) => {
        if (a.value !== undefined) return String(a.value)
        if (a.preview?.description) return a.preview.description
        if (a.description) return a.description
        return `[${a.type}]`
      })
      .join(' ')
    appendConsole({ ts: Math.round(p.timestamp * 1000), type: p.type, text, args: p.args, stackTrace: p.stackTrace })
  })

  Runtime.exceptionThrown((p) => {
    appendException({
      ts: Math.round(p.timestamp * 1000),
      text: p.exceptionDetails.text,
      exception: p.exceptionDetails.exception,
      stackTrace: p.exceptionDetails.stackTrace
    })
  })

  // ── Screencast frame relay ─────────────────────────────────────────────────

  Page.screencastFrame((p) => {
    if (!sink.isDestroyed()) {
      sink.send('external:screencast-frame', { data: p.data, metadata: p.metadata, sessionId: p.sessionId })
    }
    // Ack immediately so Chrome sends the next frame
    void Page.screencastFrameAck({ sessionId: p.sessionId }).catch(() => {})
  })

  const target: ExternalTarget = {
    async navigate(url: string) {
      await Page.navigate({ url })
    },

    async startScreencast(opts) {
      await Page.startScreencast({
        format: 'jpeg',
        quality: opts.quality ?? 80,
        everyNthFrame: opts.everyNthFrame ?? 1,
        maxWidth: opts.maxWidth ?? 1280,
        maxHeight: opts.maxHeight ?? 800
      })
    },

    async stopScreencast() {
      await Page.stopScreencast().catch(() => {})
    },

    async ackScreencast(sessionId: number) {
      await Page.screencastFrameAck({ sessionId }).catch(() => {})
    },

    async dispatchMouseEvent(params: unknown) {
      const input = client as unknown as {
        Input: { dispatchMouseEvent: (p: unknown) => Promise<unknown> }
      }
      await input.Input.dispatchMouseEvent(params)
    },

    async dispatchKeyEvent(params: unknown) {
      const input = client as unknown as {
        Input: { dispatchKeyEvent: (p: unknown) => Promise<unknown> }
      }
      await input.Input.dispatchKeyEvent(params)
    }
  }

  externalTarget = target
  return target
}

export async function detachExternalCdp(): Promise<void> {
  if (externalTarget) {
    await externalTarget.stopScreencast().catch(() => {})
    externalTarget = null
  }
  if (externalClient) {
    await externalClient.close().catch(() => {})
    externalClient = null
  }
}

export function getExternalTarget(): ExternalTarget | null {
  return externalTarget
}
