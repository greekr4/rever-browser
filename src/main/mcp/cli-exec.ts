import {
  OUTCOME_UNKNOWN,
  buildCliRequest,
  buildExtractExpression,
  compileCli,
  parseExtract,
  presentRawBody,
  runGate,
  scrubExtractItems,
  scrubSecrets,
  staticAsset,
  type CapturedCall,
  type CliCommand,
  type CliData,
  type CliEffect,
  type CliExtract,
  type CompileOptions
} from '../../shared/cli-spec'
import { getActiveTarget } from '../chrome-cdp'
import { repeaterSendRaw } from '../repeater'
import { getRequest, listRequests } from '../traffic-store'
import { scopeBlockForUrl } from './scope'

export interface CliRunResult {
  ok: boolean
  error?: string
  status?: number
  body?: string
  /** True when an HTML body was shortened so the page is not the result. */
  bodyCut?: boolean
  items?: Array<Record<string, string>>
  matched?: number
  note?: string
}

export function compileCaptured(ids: string[], opts: CompileOptions = {}): CliData {
  const calls: CapturedCall[] = []
  for (const id of ids) {
    const row = getRequest(id)
    if (!row) continue
    calls.push({
      requestId: row.requestId,
      method: row.method,
      url: row.url,
      resourceType: row.resourceType,
      status: row.status,
      mimeType: row.mimeType,
      requestHeaders: row.requestHeaders,
      requestPostData: row.requestPostData,
      responseBody: row.responseBodyBase64 ? undefined : row.responseBody
    })
  }
  return compileCli(calls, opts)
}

interface ArmWindow {
  startedAt: number
  stoppedAt: number | null
}

let armed: ArmWindow | null = null

export interface ArmedCall {
  requestId: string
  method: string
  url: string
}

export function armCli(action: 'start' | 'stop' | 'list'): {
  recording: boolean
  startedAt: number | null
  stoppedAt: number | null
  requests: ArmedCall[]
} {
  if (action === 'start') armed = { startedAt: Date.now(), stoppedAt: null }
  else if (action === 'stop' && armed && armed.stoppedAt == null) {
    armed = { ...armed, stoppedAt: Date.now() }
  }
  const window = armed
  return {
    recording: Boolean(window && window.stoppedAt == null),
    startedAt: window?.startedAt ?? null,
    stoppedAt: window?.stoppedAt ?? null,
    requests: window ? callsInWindow(window) : []
  }
}

function callsInWindow(window: ArmWindow): ArmedCall[] {
  const end = window.stoppedAt ?? Number.POSITIVE_INFINITY
  const rows = listRequests({ since: window.startedAt, limit: 200 })
  const out: ArmedCall[] = []
  for (const row of rows) {
    if (row.startedAt > end) continue
    const type = row.resourceType.toLowerCase()
    if (type !== 'xhr' && type !== 'fetch') continue
    try {
      if (staticAsset(new URL(row.url).pathname)) continue
    } catch {
      continue
    }
    out.push({ requestId: row.requestId, method: row.method, url: row.url })
  }
  return out
}

/** Header values copied into the in-page fetch only. Never returned to the model. */
export function sessionHeaderValues(origin: string, names: string[]): Record<string, string> {
  if (!names.length) return {}
  let base: URL
  try {
    base = new URL(origin)
  } catch {
    return {}
  }
  const want = new Set(names.map((name) => name.toLowerCase()))
  const found: Record<string, string> = {}
  for (const row of listRequests({ host: base.host, limit: 200 })) {
    try {
      if (new URL(row.url).origin !== base.origin) continue
    } catch {
      continue
    }
    for (const [key, value] of Object.entries(row.requestHeaders ?? {})) {
      const lower = key.toLowerCase()
      if (!want.has(lower) || !value || found[lower]) continue
      found[lower] = value
    }
    if (Object.keys(found).length === want.size) break
  }
  return found
}

export async function executeCli(input: {
  baseUrl: string
  writes: boolean
  command: CliCommand
  inputs: Record<string, string>
}): Promise<CliRunResult> {
  const blocked = runGate(input.command.effect, input.writes)
  if (blocked) return { ok: false, error: blocked }
  let built: ReturnType<typeof buildCliRequest>
  try {
    built = buildCliRequest(input.baseUrl, input.command, input.inputs)
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) }
  }
  if (!built.ok) return { ok: false, error: built.error }

  const headers: Record<string, string> = {}
  if (built.request.body && input.command.body?.json) headers['content-type'] = 'application/json'
  else if (built.request.body && input.command.body?.form) {
    headers['content-type'] = 'application/x-www-form-urlencoded'
  }
  Object.assign(headers, sessionHeaderValues(input.baseUrl, input.command.sessionHeaders))

  const parsedExtract = parseExtract(input.command.extract)
  if (!parsedExtract.ok) return { ok: false, error: parsedExtract.error }
  if (parsedExtract.extract) {
    return executeExtract({
      effect: input.command.effect,
      url: built.request.url,
      method: built.request.method,
      headers,
      ...(built.request.body ? { body: built.request.body } : {}),
      extract: parsedExtract.extract
    })
  }

  let response
  try {
    response = await repeaterSendRaw({
      url: built.request.url,
      method: built.request.method,
      headers,
      ...(built.request.body ? { body: built.request.body } : {})
    })
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) }
  }

  if (response.error && input.command.effect !== 'read') {
    return { ok: false, error: OUTCOME_UNKNOWN }
  }
  const presented = presentRawBody(scrubSecrets(response.body ?? ''), response.bodyTruncated)
  const cut = presented.bodyCut ? { bodyCut: true as const } : {}
  if (response.error) {
    return { ok: false, error: scrubSecrets(response.error), status: response.status, body: presented.body, ...cut }
  }
  return { ok: true, status: response.status, body: presented.body, ...cut }
}

interface ExtractPageResult {
  status?: number
  matched?: number
  items?: unknown
  note?: unknown
  error?: unknown
}

function asItems(value: unknown): Array<Record<string, string>> {
  if (!Array.isArray(value)) return []
  const out: Array<Record<string, string>> = []
  for (const row of value) {
    if (!row || typeof row !== 'object' || Array.isArray(row)) continue
    const item: Record<string, string> = {}
    for (const [key, field] of Object.entries(row as Record<string, unknown>)) {
      if (typeof field === 'string') item[key] = field
    }
    out.push(item)
  }
  return out
}

/** Read the full response in the tab and return rows. The page itself stays in the tab. */
async function executeExtract(input: {
  effect: CliEffect
  url: string
  method: string
  headers: Record<string, string>
  body?: string
  extract: CliExtract
}): Promise<CliRunResult> {
  const scopeError = scopeBlockForUrl(input.url)
  if (scopeError) return { ok: false, error: scopeError }
  const target = getActiveTarget()
  if (!target) return { ok: false, error: 'no active webview attached' }

  const expression = buildExtractExpression({
    url: input.url,
    method: input.method,
    headers: input.headers,
    ...(input.body != null ? { body: input.body } : {}),
    extract: input.extract
  })

  let evaluated: {
    result: { value?: ExtractPageResult }
    exceptionDetails?: { text: string; exception?: { description?: string } }
  }
  try {
    evaluated = (await target.dbg.sendCommand('Runtime.evaluate', {
      expression,
      awaitPromise: true,
      returnByValue: true,
      timeout: 30_000
    })) as typeof evaluated
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) }
  }

  if (evaluated.exceptionDetails) {
    if (input.effect !== 'read') return { ok: false, error: OUTCOME_UNKNOWN }
    const msg = evaluated.exceptionDetails.exception?.description ?? evaluated.exceptionDetails.text
    return { ok: false, error: scrubSecrets(msg) }
  }
  const page = evaluated.result.value
  if (!page) return { ok: false, error: 'empty result' }
  if (typeof page.error === 'string' && page.error) {
    if (input.effect !== 'read') return { ok: false, error: OUTCOME_UNKNOWN }
    return { ok: false, error: scrubSecrets(page.error), ...(typeof page.status === 'number' ? { status: page.status } : {}) }
  }
  const items = scrubExtractItems(asItems(page.items), input.extract.fields).slice(0, input.extract.limit)
  const matched = typeof page.matched === 'number' ? page.matched : items.length
  const note = typeof page.note === 'string' && page.note ? scrubSecrets(page.note) : undefined
  return {
    ok: true,
    ...(typeof page.status === 'number' ? { status: page.status } : {}),
    items,
    matched,
    ...(note ? { note } : {})
  }
}
