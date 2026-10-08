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

export function getConsoleCount(): number {
  return consoleLogs.length
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

export function getExceptionCount(): number {
  return runtimeExceptions.length
}

export function getWebSocketCount(): number {
  let n = 0
  for (const id of order) {
    if (entries.get(id)?.resourceType === 'WebSocket') n++
  }
  return n
}

const MAX_ENTRIES = 500
// Per-body byte ceiling. 8MB sits above webcrack's 5MB deobfuscation limit, so
// large JS bundles (the whole point of this tool) survive intact, while a
// pathological multi-hundred-MB body can't blow up the ring buffer. Bodies past
// the cap are truncated and flagged.
const MAX_BODY_CHARS = 8 * 1024 * 1024
// 전체 바디 누적 예산: 256MB. 초과 시 가장 오래된 엔트리의 responseBody를 비워낸다.
const MAX_TOTAL_BODY_BYTES = 256 * 1024 * 1024

function capBody<T extends Partial<StoredRequest>>(req: T): T {
  if (req.responseBody != null && req.responseBody.length > MAX_BODY_CHARS) {
    return { ...req, responseBody: req.responseBody.slice(0, MAX_BODY_CHARS), responseBodyTruncated: true }
  }
  return req
}

const order: string[] = []
const entries = new Map<string, StoredRequest>()
// 현재 누적 바디 바이트 수 (UTF-16 코드 유닛 기준, JS string.length)
let totalBodyBytes = 0

function evictIfNeeded() {
  while (order.length > MAX_ENTRIES) {
    const oldest = order.shift()
    if (oldest) {
      const e = entries.get(oldest)
      if (e?.responseBody) {
        totalBodyBytes = Math.max(0, totalBodyBytes - e.responseBody.length)
      }
      entries.delete(oldest)
      // Free any WebSocket frames keyed by this requestId — otherwise the
      // wsFrames map grows forever as requests churn through the ring buffer.
      wsFrames.delete(oldest)
    }
  }
  // 전역 바디 예산 초과 시 가장 오래된 엔트리부터 responseBody를 비운다.
  // 엔트리 자체는 유지해 메타데이터(URL, status 등)는 접근 가능하게 한다.
  if (totalBodyBytes > MAX_TOTAL_BODY_BYTES) {
    for (const id of order) {
      if (totalBodyBytes <= MAX_TOTAL_BODY_BYTES) break
      const e = entries.get(id)
      if (e?.responseBody) {
        totalBodyBytes = Math.max(0, totalBodyBytes - e.responseBody.length)
        e.responseBody = undefined
        e.responseBodyTruncated = true
      }
    }
  }
}

export function upsertRequest(rawReq: Partial<StoredRequest> & { requestId: string }) {
  const req = capBody(rawReq)
  const existing = entries.get(req.requestId)
  if (existing) {
    // responseBody가 교체될 때 전역 카운터를 갱신한다.
    if (req.responseBody !== undefined) {
      const old = existing.responseBody?.length ?? 0
      const next = req.responseBody?.length ?? 0
      totalBodyBytes = Math.max(0, totalBodyBytes - old) + next
    }
    Object.assign(existing, req)
    evictIfNeeded()
    return
  }
  const newEntry: StoredRequest = {
    url: '',
    host: '',
    method: '',
    resourceType: 'Other',
    startedAt: Date.now(),
    ...req
  }
  if (newEntry.responseBody) {
    totalBodyBytes += newEntry.responseBody.length
  }
  entries.set(req.requestId, newEntry)
  order.push(req.requestId)
  evictIfNeeded()
}

export interface ListFilter {
  host?: string
  methodOrType?: string
  since?: number
  // har_export ships its whole response as one SSE event, and widely used
  // MCP clients enforce a per-event size cap (httpx2: 1MB) — over that, the
  // stream is cut with no response. Paging large traffic sets in safe-sized
  // chunks needs an upper bound, which `since` alone can't express.
  //
  // The cursor pair is exclusive: `before` is the boundary entry's
  // startedAt and `beforeId` its requestId — the boundary entry itself is
  // never re-included, so every page is guaranteed to make progress (an
  // inclusive cursor loops forever once a single entry fills a page on its
  // own). Entries sharing the boundary ms that are older than the boundary
  // are still kept — `order` position disambiguates them.
  // `before` without `beforeId` stays inclusive on the boundary ms: HAR
  // entries don't carry requestIds, so a caller paging by timestamp alone
  // can't name the boundary and would lose same-ms siblings.
  before?: number
  beforeId?: string
  limit?: number
  /** live = captured in this app (default), import = loaded by import_har, all = both. */
  source?: 'live' | 'import' | 'all'
}

function scan(
  ids: string[],
  store: Map<string, StoredRequest>,
  filter: ListFilter,
  limit: number
): StoredRequest[] {
  // Position of the boundary entry in `order` — only needed when entries
  // share the `before` millisecond and we must tell older siblings from the
  // boundary entry itself.
  const beforeIdx = filter.beforeId !== undefined ? ids.indexOf(filter.beforeId) : -1
  const result: StoredRequest[] = []
  for (let i = ids.length - 1; i >= 0 && result.length < limit; i--) {
    const e = store.get(ids[i])
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
    if (filter.before !== undefined) {
      if (e.startedAt > filter.before) continue
      if (e.startedAt === filter.before) {
        // Keep only entries strictly older than the boundary — earlier in
        // `order` means inserted earlier. If the boundary was evicted,
        // beforeId alone can't order the siblings, so exclude the boundary
        // entry itself and keep the rest (callers dedupe by requestId).
        const isBoundaryOrNewer =
          beforeIdx >= 0 ? i >= beforeIdx : e.requestId === filter.beforeId
        if (isBoundaryOrNewer) continue
      }
    }
    result.push(e)
  }
  return result
}

export function listRequests(filter: ListFilter = {}): StoredRequest[] {
  const limit = filter.limit ?? 50
  const source = filter.source ?? 'live'
  if (source === 'live') return scan(order, entries, filter, limit)
  if (source === 'import') return scan(importOrder, imported, filter, limit)
  return [...scan(order, entries, filter, limit), ...scan(importOrder, imported, filter, limit)]
    .sort((a, b) => b.startedAt - a.startedAt)
    .slice(0, limit)
}

export function getRequest(requestId: string): StoredRequest | undefined {
  return entries.get(requestId) ?? imported.get(requestId)
}

// ── Imported captures (import_har) ──────────────────────────────────────────
// Kept apart from the live ring buffer so loading a large HAR can't push out
// what the user is capturing right now. Ids are `har:<importId>:<index>`.
const MAX_IMPORTED = 5_000
const importOrder: string[] = []
const imported = new Map<string, StoredRequest>()

export function importRequests(reqs: StoredRequest[]): void {
  for (const r of reqs) {
    if (!imported.has(r.requestId)) importOrder.push(r.requestId)
    imported.set(r.requestId, capBody(r))
  }
  while (importOrder.length > MAX_IMPORTED) {
    const oldest = importOrder.shift()
    if (oldest) imported.delete(oldest)
  }
}

export function clearImports(): void {
  importOrder.length = 0
  imported.clear()
}

export function getImportCount(): number {
  return importOrder.length
}

// ── responseReceivedExtraInfo merge ─────────────────────────────────────────
// Network.responseReceived.response.headers omits Set-Cookie (verified against
// Chrome via CDP: the event's header map carries no Set-Cookie even when the
// server sets one). Chromium delivers the complete raw header block in
// Network.responseReceivedExtraInfo instead — the same event Puppeteer uses
// for rawHeaders. Ordering of the two events is not guaranteed, so whichever
// arrives first is buffered here and merged when the other lands.
//
// Redirect hops share the requestId, so a buffered block may belong to an
// earlier hop than the response being stored. The block's own statusCode is
// kept alongside so callers can tell a 302's headers from the final 200's.
const pendingExtraResponseHeaders = new Map<
  string,
  { statusCode: number; headers: Record<string, string> }
>()
const MAX_PENDING_EXTRA_RESPONSE_HEADERS = 1000

export function mergeExtraResponseHeaders(
  requestId: string,
  extraHeaders: Record<string, string>,
  statusCode: number
): void {
  const existing = entries.get(requestId)
  if (existing?.responseHeaders) {
    // responseReceived already arrived — merge in place. ExtraInfo is the
    // raw authoritative block, so it wins for shared names (Chromium folds
    // repeated Set-Cookie into one '\n'-joined value). A block whose
    // statusCode doesn't match belongs to a different hop of this requestId:
    // if its responseReceived hasn't landed yet (redirect, auth retry) it is
    // re-buffered so that event's statusCode check can pick it up; a truly
    // stale block just expires via the cap.
    if (existing.status === undefined || existing.status === statusCode) {
      existing.responseHeaders = { ...existing.responseHeaders, ...extraHeaders }
      return
    }
  }
  if (pendingExtraResponseHeaders.size >= MAX_PENDING_EXTRA_RESPONSE_HEADERS) {
    const oldest = pendingExtraResponseHeaders.keys().next().value
    if (oldest !== undefined) pendingExtraResponseHeaders.delete(oldest)
  }
  pendingExtraResponseHeaders.set(requestId, { statusCode, headers: extraHeaders })
}

export function takePendingExtraResponseHeaders(
  requestId: string,
  statusCode: number
): Record<string, string> | undefined {
  const extra = pendingExtraResponseHeaders.get(requestId)
  if (!extra) return undefined
  pendingExtraResponseHeaders.delete(requestId)
  // Only hand the block over when it describes the response that just
  // arrived — a redirect hop's block shares the requestId but not the status.
  if (extra.statusCode !== statusCode) return undefined
  return extra.headers
}

export function discardPendingExtraResponseHeaders(requestId: string): void {
  pendingExtraResponseHeaders.delete(requestId)
}

export function clearTraffic() {
  order.length = 0
  entries.clear()
  wsFrames.clear()
  pendingExtraResponseHeaders.clear()
  totalBodyBytes = 0
}
