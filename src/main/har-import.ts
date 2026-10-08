// HAR 1.2 → StoredRequest, for import_har (electron-free, vitest-testable).
// Lets every traffic tool (get_request, request_diff, export_client, ...) work
// on captures made elsewhere — DevTools, Burp, Charles, mitmproxy (`hardump`).
// Idea adapted from morluto/rea's inspect_web_network_capture (MIT).

import type { StoredRequest } from './traffic-store'

interface HarHeader {
  name: string
  value: string
}

interface HarEntry {
  startedDateTime?: string
  time?: number
  _resourceType?: string
  _initiator?: {
    type?: string
    stack?: { callFrames?: Array<{ functionName?: string; url?: string; lineNumber?: number; columnNumber?: number }> }
  }
  request?: { method?: string; url?: string; headers?: HarHeader[]; postData?: { text?: string } }
  response?: {
    status?: number
    headers?: HarHeader[]
    content?: { mimeType?: string; text?: string; encoding?: string }
    _transferSize?: number
    bodySize?: number
  }
}

export interface HarImportResult {
  entries: StoredRequest[]
  skipped: Array<{ index: number; reason: string }>
  creator: string | null
}

// Chrome's _resourceType values → the CDP casing rever stores.
const RESOURCE_TYPES: Record<string, string> = {
  document: 'Document',
  stylesheet: 'Stylesheet',
  image: 'Image',
  media: 'Media',
  font: 'Font',
  script: 'Script',
  xhr: 'XHR',
  fetch: 'Fetch',
  websocket: 'WebSocket',
  manifest: 'Manifest',
  ping: 'Ping',
  preflight: 'Preflight'
}

function guessResourceType(mimeType: string): string {
  if (/javascript|ecmascript/.test(mimeType)) return 'Script'
  if (mimeType === 'text/html') return 'Document'
  if (/json/.test(mimeType)) return 'Fetch'
  if (mimeType === 'text/css') return 'Stylesheet'
  if (mimeType.startsWith('image/')) return 'Image'
  if (mimeType.startsWith('font/') || /woff|ttf|otf/.test(mimeType)) return 'Font'
  return 'Other'
}

// Repeated fields are folded with '\n' — the CDP convention har_export's
// toHeaders() splits back apart, so an export → import round-trips.
function headerRecord(headers: HarHeader[] | undefined): Record<string, string> {
  const out: Record<string, string> = {}
  for (const h of headers ?? []) {
    if (!h || typeof h.name !== 'string') continue
    out[h.name] = h.name in out ? `${out[h.name]}\n${h.value}` : String(h.value ?? '')
  }
  return out
}

export function harToEntries(har: unknown, importId: string): HarImportResult {
  const log = (har as { log?: { entries?: unknown; creator?: { name?: string; version?: string } } } | null)?.log
  if (!log || !Array.isArray(log.entries)) throw new Error('not a HAR file (missing log.entries)')

  const entries: StoredRequest[] = []
  const skipped: HarImportResult['skipped'] = []
  log.entries.forEach((raw: HarEntry, index) => {
    const url = raw?.request?.url
    let host: string
    try {
      host = new URL(String(url)).host
    } catch {
      skipped.push({ index, reason: 'invalid request.url' })
      return
    }
    const startedAt = Date.parse(raw.startedDateTime ?? '') || 0
    const mimeType = (raw.response?.content?.mimeType ?? '').split(';')[0].trim()
    const transfer = raw.response?._transferSize ?? raw.response?.bodySize
    const frames = raw._initiator?.stack?.callFrames
    const content = raw.response?.content

    entries.push({
      requestId: `har:${importId}:${index}`,
      url: String(url),
      host,
      method: raw.request?.method ?? 'GET',
      resourceType: RESOURCE_TYPES[raw._resourceType ?? ''] ?? guessResourceType(mimeType),
      startedAt,
      ...(typeof raw.time === 'number' && raw.time >= 0 ? { completedAt: startedAt + raw.time } : {}),
      ...(raw.response?.status ? { status: raw.response.status } : {}),
      ...(mimeType ? { mimeType } : {}),
      ...(typeof transfer === 'number' && transfer >= 0 ? { encodedDataLength: transfer } : {}),
      requestHeaders: headerRecord(raw.request?.headers),
      ...(raw.request?.postData?.text != null ? { requestPostData: raw.request.postData.text } : {}),
      responseHeaders: headerRecord(raw.response?.headers),
      ...(content?.text != null ? { responseBody: content.text } : {}),
      ...(content?.text != null && content.encoding === 'base64' ? { responseBodyBase64: true } : {}),
      ...(raw._initiator?.type ? { initiatorType: raw._initiator.type } : {}),
      ...(frames?.length
        ? {
            initiatorStack: frames.map((f) => ({
              functionName: f.functionName ?? '',
              url: f.url ?? '',
              lineNumber: f.lineNumber ?? 0,
              columnNumber: f.columnNumber ?? 0
            }))
          }
        : {})
    })
  })

  const c = log.creator
  return { entries, skipped, creator: c?.name ? `${c.name}${c.version ? ` ${c.version}` : ''}` : null }
}
