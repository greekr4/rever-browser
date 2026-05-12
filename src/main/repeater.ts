import { getActiveTarget } from './chrome-cdp'
import { getRequest } from './traffic-store'

export interface RepeaterModifications {
  url?: string
  method?: string
  setHeaders?: Record<string, string>
  removeHeaders?: string[]
  body?: string | null
}

export interface RepeaterRequestSpec {
  url: string
  method: string
  headers: Record<string, string>
  body?: string
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

const MAX_BODY_BYTES = 64 * 1024

// fetch() refuses to set these. Strip them from copied request headers and from
// caller-provided overrides so the page-side fetch doesn't throw a TypeError.
const FORBIDDEN_HEADER_RE =
  /^(?:host|connection|content-length|trailer|transfer-encoding|upgrade|keep-alive|te|expect|cookie2|date|origin|referer|user-agent|accept-charset|accept-encoding|access-control-request-headers|access-control-request-method|via|sec-.+|proxy-.+)$/i

function dropForbidden(headers: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {}
  for (const [k, v] of Object.entries(headers)) {
    if (FORBIDDEN_HEADER_RE.test(k)) continue
    out[k] = v
  }
  return out
}

export function buildRequestSpec(
  requestId: string,
  mods: RepeaterModifications | undefined
): RepeaterRequestSpec {
  const stored = getRequest(requestId)
  if (!stored) throw new Error(`unknown requestId: ${requestId}`)

  const url = mods?.url ?? stored.url
  const method = (mods?.method ?? stored.method ?? 'GET').toUpperCase()

  const headers: Record<string, string> = stored.requestHeaders
    ? dropForbidden(stored.requestHeaders)
    : {}

  if (mods?.removeHeaders) {
    for (const name of mods.removeHeaders) {
      const lc = name.toLowerCase()
      for (const k of Object.keys(headers)) {
        if (k.toLowerCase() === lc) delete headers[k]
      }
    }
  }
  if (mods?.setHeaders) {
    for (const [k, v] of Object.entries(mods.setHeaders)) {
      if (FORBIDDEN_HEADER_RE.test(k)) continue
      const lc = k.toLowerCase()
      for (const existing of Object.keys(headers)) {
        if (existing.toLowerCase() === lc) delete headers[existing]
      }
      headers[k] = v
    }
  }

  let body: string | undefined
  if (mods && 'body' in mods) {
    body = mods.body == null ? undefined : mods.body
  } else {
    body = stored.requestPostData
  }

  return { url, method, headers, body }
}

export async function repeaterSendRaw(spec: RepeaterRequestSpec): Promise<RepeaterResponse> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active webview attached')

  const cleanedHeaders = dropForbidden(spec.headers)
  const expression = buildEvalExpression(
    { ...spec, headers: cleanedHeaders },
    MAX_BODY_BYTES
  )

  const result = (await target.dbg.sendCommand('Runtime.evaluate', {
    expression,
    awaitPromise: true,
    returnByValue: true,
    timeout: 30_000
  })) as {
    result: { value?: RepeaterResponse }
    exceptionDetails?: { text: string; exception?: { description?: string } }
  }

  if (result.exceptionDetails) {
    const msg = result.exceptionDetails.exception?.description ?? result.exceptionDetails.text
    return {
      status: 0,
      statusText: '',
      headers: {},
      body: '',
      bodyTruncated: false,
      bodyByteLength: 0,
      timeMs: 0,
      error: msg
    }
  }
  if (!result.result.value) throw new Error('repeater: empty result')
  return result.result.value
}

export async function repeaterSend(
  requestId: string,
  mods: RepeaterModifications | undefined
): Promise<RepeaterResponse> {
  return repeaterSendRaw(buildRequestSpec(requestId, mods))
}

function buildEvalExpression(spec: RepeaterRequestSpec, maxBytes: number): string {
  const hasBody = spec.body !== undefined && spec.method !== 'GET' && spec.method !== 'HEAD'
  return `
(async () => {
  const t0 = performance.now()
  try {
    const init = {
      method: ${JSON.stringify(spec.method)},
      headers: ${JSON.stringify(spec.headers)},
      credentials: 'include',
      redirect: 'follow',
      cache: 'no-store'
    }
    ${hasBody ? `init.body = ${JSON.stringify(spec.body)}` : ''}
    const res = await fetch(${JSON.stringify(spec.url)}, init)
    const buf = await res.arrayBuffer()
    const bytes = new Uint8Array(buf)
    const total = bytes.length
    const slice = total > ${maxBytes} ? bytes.slice(0, ${maxBytes}) : bytes
    let body
    try {
      body = new TextDecoder('utf-8', { fatal: false }).decode(slice)
    } catch (_e) {
      body = '[binary; ' + total + ' bytes]'
    }
    const headers = {}
    res.headers.forEach((v, k) => { headers[k] = v })
    return {
      status: res.status,
      statusText: res.statusText,
      headers,
      body,
      bodyTruncated: total > ${maxBytes},
      bodyByteLength: total,
      timeMs: Math.round(performance.now() - t0)
    }
  } catch (e) {
    return {
      status: 0,
      statusText: '',
      headers: {},
      body: '',
      bodyTruncated: false,
      bodyByteLength: 0,
      timeMs: Math.round(performance.now() - t0),
      error: (e && e.message) ? e.message : String(e)
    }
  }
})()
`
}
