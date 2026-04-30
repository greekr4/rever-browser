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
  responseBodyError?: string
}

const MAX_ENTRIES = 500

const order: string[] = []
const entries = new Map<string, StoredRequest>()

function evictIfNeeded() {
  while (order.length > MAX_ENTRIES) {
    const oldest = order.shift()
    if (oldest) entries.delete(oldest)
  }
}

export function upsertRequest(req: Partial<StoredRequest> & { requestId: string }) {
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
