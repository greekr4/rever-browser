export interface StoredRequest {
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
  responseBodyTruncated?: boolean
  responseBodyError?: string
  // initiator info
  initiatorType?: string
  initiatorStack?: Array<{
    functionName: string
    url: string
    lineNumber: number
    columnNumber: number
  }>
  initiatorUrl?: string
}

// ── WebSocket frames ────────────────────────────────────────────────────────

export interface WSFrame {
  direction: 'sent' | 'received'
  opcode: number
  payloadData: string
  timestamp: number
  mask?: boolean
}

const wsFrames = new Map<string, WSFrame[]>()
// Cap frames per request so a single long-lived socket can't grow unbounded
// even while its request stays inside the ring buffer.
const MAX_WS_FRAMES_PER_REQUEST = 2000

export function appendWsFrame(requestId: string, frame: WSFrame): void {
  let frames = wsFrames.get(requestId)
  if (!frames) {
    frames = []
    wsFrames.set(requestId, frames)
  }
  frames.push(frame)
  if (frames.length > MAX_WS_FRAMES_PER_REQUEST) frames.shift()
}

export function getWsFrames(requestId: string, since?: number): WSFrame[] {
  const frames = wsFrames.get(requestId) ?? []
  if (since == null) return frames
  return frames.filter((f) => f.timestamp >= since)
}

// ── Console logs ────────────────────────────────────────────────────────────

export interface ConsoleEntry {
  ts: number
  type: string
  text: string
  args?: unknown[]
  stackTrace?: unknown
}

const MAX_CONSOLE = 1000
const consoleLogs: ConsoleEntry[] = []

export function appendConsole(entry: ConsoleEntry): void {
  consoleLogs.push(entry)
  if (consoleLogs.length > MAX_CONSOLE) consoleLogs.shift()
}

export function getConsoleSince(since?: number): ConsoleEntry[] {
  if (since == null) return [...consoleLogs]
  return consoleLogs.filter((e) => e.ts >= since)
}

export function clearConsole(): void {
  consoleLogs.length = 0
}

// ── Runtime exceptions ───────────────────────────────────────────────────────

export interface RuntimeException {
  ts: number
  text: string
  exception?: unknown
  stackTrace?: unknown
}

const MAX_EXCEPTIONS = 200
const runtimeExceptions: RuntimeException[] = []

export function appendException(entry: RuntimeException): void {
  runtimeExceptions.push(entry)
  if (runtimeExceptions.length > MAX_EXCEPTIONS) runtimeExceptions.shift()
}

export function getExceptions(): RuntimeException[] {
  return [...runtimeExceptions]
}

const MAX_ENTRIES = 500
// Per-body byte ceiling. 8MB sits above webcrack's 5MB deobfuscation limit, so
// large JS bundles (the whole point of this tool) survive intact, while a
// pathological multi-hundred-MB body can't blow up the ring buffer. Bodies past
// the cap are truncated and flagged.
const MAX_BODY_CHARS = 8 * 1024 * 1024

function capBody<T extends Partial<StoredRequest>>(req: T): T {
  if (req.responseBody != null && req.responseBody.length > MAX_BODY_CHARS) {
    return { ...req, responseBody: req.responseBody.slice(0, MAX_BODY_CHARS), responseBodyTruncated: true }
  }
  return req
}

const order: string[] = []
const entries = new Map<string, StoredRequest>()

function evictIfNeeded() {
  while (order.length > MAX_ENTRIES) {
    const oldest = order.shift()
    if (oldest) {
      entries.delete(oldest)
      // Free any WebSocket frames keyed by this requestId — otherwise the
      // wsFrames map grows forever as requests churn through the ring buffer.
      wsFrames.delete(oldest)
    }
  }
}

export function upsertRequest(rawReq: Partial<StoredRequest> & { requestId: string }) {
  const req = capBody(rawReq)
  const existing = entries.get(req.requestId)
  if (existing) {
    Object.assign(existing, req)
    return
  }
  entries.set(req.requestId, {
    url: '',
    host: '',
    method: '',
    resourceType: 'Other',
    startedAt: Date.now(),
    ...req
  })
  order.push(req.requestId)
  evictIfNeeded()
}

export interface ListFilter {
  host?: string
  methodOrType?: string
  since?: number
  limit?: number
}

export function listRequests(filter: ListFilter = {}): StoredRequest[] {
  const limit = filter.limit ?? 50
  const result: StoredRequest[] = []
  for (let i = order.length - 1; i >= 0 && result.length < limit; i--) {
    const e = entries.get(order[i])
    if (!e) continue
    if (filter.host && !e.host.includes(filter.host)) continue
    if (filter.methodOrType) {
      const needle = filter.methodOrType.toLowerCase()
      if (
        !e.method.toLowerCase().includes(needle) &&
        !e.resourceType.toLowerCase().includes(needle)
      ) {
        continue
      }
    }
    if (filter.since && e.startedAt < filter.since) continue
    result.push(e)
  }
  return result
}

export function getRequest(requestId: string): StoredRequest | undefined {
  return entries.get(requestId)
}

export function clearTraffic() {
  order.length = 0
  entries.clear()
}
