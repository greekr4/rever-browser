// Captured calls become a site CLI. Algorithms follow Hybirdss/Hycli
// (Apache-2.0) src/capture/mod.rs and src/policy.rs: cluster a recording,
// template dynamic path segments, drop static assets and secret field names,
// and classify effect without trusting a caller-supplied label.
// The fork greekr4/Hycli matched that upstream at the time of the port.
// Values, cookies, and machine paths are not stored.

export type CliEffect = 'read' | 'write' | 'unknown'

export interface CliParam {
  kind: 'string' | 'int' | 'bool'
  required: boolean
}

/** One value read from each row the item selector matches. */
export interface CliExtractField {
  name: string
  /** CSS selector relative to the row. Omit to use the row itself. */
  selector?: string
  /** Attribute name. Omit to read the text. */
  attr?: string
}

/** Fields to pull out of an HTML response instead of returning the page. */
export interface CliExtract {
  selector: string
  /** Default 5. Maximum 20. */
  limit: number
  fields: CliExtractField[]
}

export interface CliCommand {
  name: string
  desc: string
  method: string
  path: string
  effect: CliEffect
  evidenceRequestIds: string[]
  params: Record<string, CliParam>
  query: Record<string, CliParam>
  body?: { json?: Record<string, CliParam>; form?: Record<string, CliParam> }
  sessionHeaders: string[]
  responsePointers: string[]
  /** Saved once. Later runs return these fields instead of the raw page. */
  extract?: CliExtract
}

export interface CliData {
  site: string
  title: string
  baseUrl: string
  commands: CliCommand[]
  writes: boolean
}

export interface CapturedCall {
  requestId: string
  method: string
  url: string
  resourceType?: string
  status?: number
  mimeType?: string
  requestHeaders?: Record<string, string>
  requestPostData?: string
  responseBody?: string
}

export interface CompileOptions {
  site?: string
  title?: string
  baseUrl?: string
  /** Default 20. Values below 1 or above 100 become 20. */
  limit?: number
  /** Command name per requestId, e.g. { "123.4": "search" }. Slugged; empty falls back. */
  names?: Record<string, string>
}

export const WRITES_DISABLED = 'writes disabled for this CLI'
export const OUTCOME_UNKNOWN = 'action_outcome_unknown'

const STATIC_EXT = [
  '.js',
  '.css',
  '.png',
  '.jpg',
  '.jpeg',
  '.gif',
  '.svg',
  '.woff',
  '.woff2',
  '.ico',
  '.map'
]

const DROPPED_TYPES = new Set(['script', 'stylesheet', 'image', 'font', 'media', 'texttrack', 'manifest'])

const CHANGES = new Set([
  'add',
  'edit',
  'change',
  'toggle',
  'commit',
  'upsert',
  'redeem',
  'confirm',
  'authorize',
  'login',
  'signin',
  'logout',
  'logoff',
  'signout',
  'delete',
  'remove',
  'destroy',
  'erase',
  'purge',
  'drop',
  'create',
  'insert',
  'update',
  'patch',
  'submit',
  'send',
  'publish',
  'unpublish',
  'upload',
  'transfer',
  'purchase',
  'checkout',
  'pay',
  'vote',
  'like',
  'unlike',
  'follow',
  'unfollow',
  'subscribe',
  'unsubscribe',
  'revoke',
  'reset',
  'activate',
  'deactivate',
  'enable',
  'disable',
  'execute',
  'trigger',
  'accept',
  'reject',
  'approve',
  'invite',
  'cancel',
  'terminate',
  'stop',
  'restart',
  'save',
  'markread',
  'markseen',
  'increment'
])

const SECRET_WORDS = [
  'tokens',
  'cookies',
  'session',
  'auth',
  'token',
  'authtoken',
  'csrf',
  'xsrf',
  'credential',
  'credentials',
  'password',
  'passwd',
  'authorization',
  'cookie',
  'setcookie',
  'accesstoken',
  'refreshtoken',
  'idtoken',
  'csrftoken',
  'xsrftoken',
  'apikey',
  'secret',
  'clientsecret',
  'sessiontoken',
  'sessionid',
  'bearer'
]

const BEARER_RE = /\bBearer\s+[A-Za-z0-9._~+/-]{8,}=*/g
const JWT_RE = /eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/g

export function dangerousIntent(value: string): boolean {
  let decoded = value
  try {
    decoded = decodeURIComponent(value.replace(/\+/g, ' '))
  } catch {
    decoded = value
  }
  const lower = decoded.replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase()
  const words = lower.split(/[^a-z0-9]+/).filter(Boolean)
  if (words.some((word) => CHANGES.has(word))) return true
  return ['sign-out', 'log-out', 'mark-as-read', 'mark-as-seen'].some((word) => lower.includes(word))
}

export function secretField(key: string): boolean {
  const lower = key.toLowerCase().replace(/[-_]/g, '')
  return SECRET_WORDS.some((word) => lower === word || lower.endsWith(word))
}

export function staticAsset(path: string): boolean {
  const lower = path.toLowerCase()
  return STATIC_EXT.some((ext) => lower.endsWith(ext))
}

export function scrubSecrets(text: string): string {
  return text.replace(BEARER_RE, 'Bearer ****').replace(JWT_RE, '****')
}

export function runGate(effect: CliEffect, writes: boolean): string | null {
  if (effect !== 'read' && !writes) return WRITES_DISABLED
  return null
}

function limitOf(limit: number | undefined): number {
  if (limit == null || limit < 1 || limit > 100) return 20
  return limit
}

function slugSite(host: string): string {
  const slug = host.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
  return slug || 'site'
}

function isDynamic(seg: string): boolean {
  if (!seg) return false
  if (/^\d+$/.test(seg)) return true
  if (seg.length === 36 && seg.split('-').length === 5) return true
  return seg.length >= 16 && /^[0-9a-f]+$/i.test(seg)
}

function templify(pathname: string): string {
  let idx = 0
  return pathname
    .split('/')
    .map((seg) => {
      if (!seg || !isDynamic(seg)) return seg
      const name = `id${idx}`
      idx += 1
      return `{${name}}`
    })
    .join('/')
}

function slugify(method: string, template: string): string {
  const body = template
    .replace(/[{}/]/g, '-')
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, '')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '')
  return `${method.toLowerCase()}-${body || 'root'}`
}

function commandSlug(raw: string): string {
  return raw
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '')
    .slice(0, 40)
    .replace(/-$/, '')
}

function chosenName(ids: string[], names: Record<string, string> | undefined): string {
  if (!names) return ''
  for (const id of ids) {
    const raw = names[id]
    if (raw) return commandSlug(raw)
  }
  return ''
}

function paramKind(value: string): CliParam['kind'] {
  if (/^-?\d+$/.test(value)) return 'int'
  if (value === 'true' || value === 'false') return 'bool'
  return 'string'
}

function headerName(name: string): string | null {
  const lower = name.toLowerCase()
  if (lower === 'cookie' || lower === 'authorization') return null
  if (!/csrf|xsrf/.test(lower)) return null
  if (!/^[a-z0-9-]{1,80}$/.test(lower)) return null
  return lower
}

function jsonPointers(body: string | undefined): string[] {
  if (!body) return []
  try {
    const parsed = JSON.parse(body) as unknown
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return []
    return Object.keys(parsed)
      .filter((key) => !secretField(key))
      .slice(0, 12)
      .map((key) => `/${key}`)
  } catch {
    return []
  }
}

function looksJson(call: CapturedCall): boolean {
  if ((call.mimeType ?? '').toLowerCase().includes('json')) return true
  const body = call.responseBody?.trim() ?? ''
  return body.startsWith('{') || body.startsWith('[')
}

interface Cluster {
  method: string
  origin: string
  template: string
  firstIndex: number
  count: number
  lastStatus: number
  isJson: boolean
  ids: string[]
  query: Map<string, CliParam['kind']>
  jsonBody: Map<string, CliParam['kind']>
  formBody: Map<string, CliParam['kind']>
  sessionHeaders: Set<string>
  pointers: string[]
  rawForEffect: string
}

function addFields(target: Map<string, CliParam['kind']>, fields: Array<[string, string]>): void {
  for (const [key, value] of fields) {
    if (!key || secretField(key) || target.has(key)) continue
    target.set(key, paramKind(value))
  }
}

function effectFor(method: string, template: string, name: string, raw: string): CliEffect {
  if (dangerousIntent(template) || dangerousIntent(name) || dangerousIntent(raw)) return 'write'
  const upper = method.toUpperCase()
  if (upper === 'PUT' || upper === 'PATCH' || upper === 'DELETE') return 'write'
  if (/\bmutation\b/.test(raw)) return 'write'
  if (upper === 'GET' || upper === 'HEAD' || upper === 'OPTIONS') return 'read'
  if (upper === 'POST') return 'write'
  return 'unknown'
}

function absorb(cluster: Cluster, call: CapturedCall, url: URL): void {
  cluster.count += 1
  cluster.lastStatus = call.status ?? cluster.lastStatus
  cluster.ids.push(call.requestId)
  if (looksJson(call)) cluster.isJson = true
  addFields(cluster.query, [...url.searchParams.entries()])
  const headers = call.requestHeaders ?? {}
  for (const name of Object.keys(headers)) {
    const kept = headerName(name)
    if (kept) cluster.sessionHeaders.add(kept)
  }
  const pointers = jsonPointers(call.responseBody)
  if (pointers.length > cluster.pointers.length) cluster.pointers = pointers
  const post = call.requestPostData ?? ''
  cluster.rawForEffect += `\n${url.search}\n${post}`
  if (!post) return
  const contentType = Object.entries(headers).find(([key]) => key.toLowerCase() === 'content-type')?.[1] ?? ''
  if (contentType.includes('application/x-www-form-urlencoded') || (!post.trim().startsWith('{') && post.includes('='))) {
    addFields(cluster.formBody, [...new URLSearchParams(post).entries()])
    return
  }
  try {
    const parsed = JSON.parse(post) as unknown
    if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
      const fields: Array<[string, string]> = []
      for (const [key, value] of Object.entries(parsed)) {
        if (value == null || typeof value === 'object') fields.push([key, ''])
        else fields.push([key, String(value)])
      }
      addFields(cluster.jsonBody, fields)
    }
  } catch {
    // A body that is not a field map stays out of the flags.
  }
}

export function compileCli(calls: CapturedCall[], opts: CompileOptions = {}): CliData {
  const limit = limitOf(opts.limit)
  let originFilter = ''
  if (opts.baseUrl) {
    originFilter = new URL(opts.baseUrl).origin
  }
  const groups = new Map<string, Cluster>()
  calls.forEach((call, index) => {
    if (!call.requestId.trim()) return
    let url: URL
    try {
      url = new URL(call.url)
    } catch {
      return
    }
    if (originFilter && url.origin !== originFilter) return
    const type = (call.resourceType ?? '').toLowerCase()
    if (type && DROPPED_TYPES.has(type)) return
    if (staticAsset(url.pathname)) return
    const method = (call.method || 'GET').toUpperCase()
    const template = templify(url.pathname || '/')
    const key = `${method} ${url.origin} ${template}`
    let cluster = groups.get(key)
    if (!cluster) {
      cluster = {
        method,
        origin: url.origin,
        template,
        firstIndex: index,
        count: 0,
        lastStatus: call.status ?? 0,
        isJson: false,
        ids: [],
        query: new Map(),
        jsonBody: new Map(),
        formBody: new Map(),
        sessionHeaders: new Set(),
        pointers: [],
        rawForEffect: ''
      }
      groups.set(key, cluster)
    }
    absorb(cluster, call, url)
  })

  let clusters = [...groups.values()]
  if (clusters.length > limit) {
    clusters = clusters
      .sort((a, b) => Number(b.isJson) - Number(a.isJson) || a.firstIndex - b.firstIndex)
      .slice(0, limit)
  } else {
    clusters.sort((a, b) => a.firstIndex - b.firstIndex)
  }

  const base = opts.baseUrl ? new URL(opts.baseUrl).origin : clusters[0]?.origin ?? ''
  const site = slugSite(opts.site || (base ? new URL(base).host : 'site'))
  const used = new Set<string>()
  const commands: CliCommand[] = []
  for (const cluster of clusters) {
    if (cluster.ids.length === 0) continue
    // The effect is judged on the derived name, so a friendly name chosen by
    // the caller can never turn a write into a read.
    const derived = slugify(cluster.method, cluster.template)
    let name = chosenName(cluster.ids, opts.names) || derived
    while (used.has(name)) name += '-x'
    used.add(name)
    const toParams = (fields: Map<string, CliParam['kind']>, required: boolean): Record<string, CliParam> => {
      const out: Record<string, CliParam> = {}
      for (const [key, kind] of fields) out[key] = { kind, required }
      return out
    }
    const params: Record<string, CliParam> = {}
    for (const match of cluster.template.matchAll(/\{([^}]+)\}/g)) {
      const key = match[1]
      if (key && !secretField(key)) params[key] = { kind: 'string', required: true }
    }
    const json = toParams(cluster.jsonBody, false)
    const form = toParams(cluster.formBody, false)
    const body =
      Object.keys(json).length > 0 ? { json } : Object.keys(form).length > 0 ? { form } : undefined
    commands.push({
      name,
      desc: `Observed ${cluster.method} ${cluster.count} time(s), last status ${cluster.lastStatus}.`,
      method: cluster.method,
      path: cluster.template,
      effect: effectFor(cluster.method, cluster.template, derived, cluster.rawForEffect),
      evidenceRequestIds: cluster.ids,
      params,
      query: toParams(cluster.query, false),
      ...(body ? { body } : {}),
      sessionHeaders: [...cluster.sessionHeaders].sort(),
      responsePointers: cluster.pointers
    })
  }

  return {
    site,
    title: opts.title || site,
    baseUrl: base,
    commands,
    writes: false
  }
}

function flagLines(command: CliCommand): string[] {
  const lines: string[] = []
  const emit = (prefix: string, fields: Record<string, CliParam> | undefined) => {
    for (const [name, param] of Object.entries(fields ?? {})) {
      lines.push(`  --${name} ${param.kind}${param.required ? ' required' : ''} ${prefix}`.trimEnd())
    }
  }
  emit('path', command.params)
  emit('query', command.query)
  emit('json', command.body?.json)
  emit('form', command.body?.form)
  return lines
}

export function commandFlagNames(command: CliCommand): string[] {
  return [
    ...Object.keys(command.params),
    ...Object.keys(command.query),
    ...Object.keys(command.body?.json ?? {}),
    ...Object.keys(command.body?.form ?? {})
  ]
}

/** A flag value safe to show in the panel. Secrets and cookies never qualify. */
function safeFlagValue(value: string): string | null {
  const trimmed = value.trim()
  if (!trimmed || trimmed.length > 500) return null
  if (scrubSecrets(trimmed) !== trimmed) return null
  if (/[\u0000-\u001f]/.test(trimmed)) return null
  return trimmed
}

export interface CapturedFlagSource {
  url: string
  requestPostData?: string
  requestHeaders?: Record<string, string>
}

/**
 * Flag values still visible on the captured call. Used to fill the call example
 * in the panel. Not written onto the saved command.
 */
export function capturedFlagValues(command: CliCommand, calls: CapturedFlagSource[]): Record<string, string> {
  const names = new Set(commandFlagNames(command))
  const out: Record<string, string> = {}
  const take = (key: string, value: string): void => {
    if (!names.has(key) || out[key] != null || secretField(key)) return
    const safe = safeFlagValue(value)
    if (safe == null) return
    out[key] = safe
  }
  for (const call of calls) {
    let url: URL
    try {
      url = new URL(call.url)
    } catch {
      continue
    }
    const templateSegs = command.path.split('/')
    const pathSegs = url.pathname.split('/')
    if (templateSegs.length === pathSegs.length) {
      for (let i = 0; i < templateSegs.length; i++) {
        const match = /^\{([^}]+)\}$/.exec(templateSegs[i] ?? '')
        if (match?.[1]) {
          let decoded = pathSegs[i] ?? ''
          try {
            decoded = decodeURIComponent(decoded)
          } catch {
            // A broken escape stays as the raw segment.
          }
          take(match[1], decoded)
        }
      }
    }
    for (const [key, value] of url.searchParams) take(key, value)
    const post = call.requestPostData ?? ''
    if (!post) continue
    const contentType =
      Object.entries(call.requestHeaders ?? {}).find(([key]) => key.toLowerCase() === 'content-type')?.[1] ?? ''
    if (contentType.includes('application/x-www-form-urlencoded') || (!post.trim().startsWith('{') && post.includes('='))) {
      for (const [key, value] of new URLSearchParams(post)) take(key, value)
      continue
    }
    try {
      const parsed = JSON.parse(post) as unknown
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        for (const [key, value] of Object.entries(parsed)) {
          if (value == null || typeof value === 'object') continue
          take(key, String(value))
        }
      }
    } catch {
      // A body that is not a field map adds no example values.
    }
  }
  return out
}

/** The cli_run object for one command. Blank flags stay empty strings. No cookies. */
export function cliRunExample(id: string, command: CliCommand, inputs: Record<string, string>): string {
  const filled: Record<string, string> = {}
  for (const name of commandFlagNames(command)) filled[name] = inputs[name] ?? ''
  return JSON.stringify({ id, command: command.name, inputs: filled }, null, 2)
}

export function describeCli(data: CliData, commandName?: string): string {
  const commands = commandName ? data.commands.filter((command) => command.name === commandName) : data.commands
  const lines = [`${data.site}  ${data.baseUrl}`, `writes: ${data.writes ? 'on' : 'off'}`, '']
  if (commandName && commands.length === 0) {
    lines.push(`Unknown action: ${commandName}`)
    lines.push(`Available actions: ${data.commands.map((command) => command.name).join(', ')}`)
    return lines.join('\n')
  }
  for (const command of commands) {
    lines.push(command.name)
    lines.push(`  ${command.method} ${command.path}`)
    lines.push(`  effect: ${command.effect}`)
    if (command.extract) {
      lines.push(`  extract: ${command.extract.selector}  limit ${command.extract.limit}`)
      for (const field of command.extract.fields) {
        const how = field.attr ? `attr ${field.attr}` : 'text'
        lines.push(`    ${field.name} ${how}${field.selector ? ` ${field.selector}` : ''}`)
      }
    }
    lines.push(...flagLines(command))
    if (command.sessionHeaders.length) lines.push(`  session headers: ${command.sessionHeaders.join(', ')}`)
    lines.push(`  evidence: ${command.evidenceRequestIds.join(', ')}`)
    lines.push('')
  }
  return lines.join('\n')
}

export function exportCurl(data: CliData): string {
  const lines = [
    `# ${data.site} ${data.baseUrl}`,
    `# writes: ${data.writes ? 'on' : 'off'}`,
    '# BASE is the site origin. Flags are placeholders. Captured secrets are not included.',
    '# UA is your client\'s User-Agent, e.g. "my-tool/1.0 (you@example.com)". Some APIs require one.',
    ''
  ]
  for (const command of data.commands) {
    const path = command.path.replace(/\{([^}]+)\}/g, (_match, name: string) => `$${name}`)
    const query = Object.keys(command.query)
      .map((name) => `${encodeURIComponent(name)}=$${name}`)
      .join('&')
    const url = `$BASE${path}${query ? `?${query}` : ''}`
    let extra = ''
    if (command.body?.json && Object.keys(command.body.json).length) {
      const pairs = Object.keys(command.body.json)
        .map((name) => `"${name}":"$${name}"`)
        .join(',')
      extra = ` -H 'content-type: application/json' --data '{${pairs}}'`
    } else if (command.body?.form && Object.keys(command.body.form).length) {
      const pairs = Object.keys(command.body.form)
        .map((name) => `${encodeURIComponent(name)}=$${name}`)
        .join('&')
      extra = ` --data '${pairs}'`
    }
    lines.push(`# ${command.name}  ${command.method} ${command.path}  ${command.effect}`)
    if (command.extract) {
      const names = command.extract.fields.map((field) => field.name).join(', ')
      lines.push(`# reads ${command.extract.limit}: ${names}`)
      lines.push(`# selector: ${command.extract.selector}`)
    }
    lines.push(`curl -sS -A "$UA" -X ${command.method} "${url}"${extra}`)
    lines.push('')
  }
  return lines.join('\n')
}

export interface BuiltCliRequest {
  url: string
  method: string
  body?: string
}

export function buildCliRequest(
  baseUrl: string,
  command: CliCommand,
  inputs: Record<string, string>
): { ok: true; request: BuiltCliRequest } | { ok: false; error: string } {
  let path = command.path
  for (const name of Object.keys(command.params)) {
    const value = inputs[name]
    if (!value) return { ok: false, error: `missing ${name}` }
    path = path.replaceAll(`{${name}}`, encodeURIComponent(value))
  }
  const url = new URL(path, baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`)
  for (const [name, param] of Object.entries(command.query)) {
    const value = inputs[name]
    if (value) url.searchParams.set(name, value)
    else if (param.required) return { ok: false, error: `missing ${name}` }
  }
  let body: string | undefined
  const json = command.body?.json
  const form = command.body?.form
  if (json && Object.keys(json).length) {
    const payload: Record<string, string> = {}
    for (const [name, param] of Object.entries(json)) {
      const value = inputs[name]
      if (value) payload[name] = value
      else if (param.required) return { ok: false, error: `missing ${name}` }
    }
    if (Object.keys(payload).length) body = JSON.stringify(payload)
  } else if (form && Object.keys(form).length) {
    const payload = new URLSearchParams()
    for (const [name, param] of Object.entries(form)) {
      const value = inputs[name]
      if (value) payload.set(name, value)
      else if (param.required) return { ok: false, error: `missing ${name}` }
    }
    if ([...payload.keys()].length) body = payload.toString()
  }
  return { ok: true, request: { url: url.toString(), method: command.method, ...(body ? { body } : {}) } }
}

export const RAW_PREVIEW_CHARS = 1_500
export const EXTRACT_MAX_CHARS = 2_000_000
export const EXTRACT_FETCH_TIMEOUT_MS = 15_000
export const EXTRACT_NOTE_NONE = 'selector matched nothing'
export const EXTRACT_NOTE_CAPPED = 'page read was capped'
const FIELD_VALUE_MAX = 500
const FIELD_NAME_RE = /^[A-Za-z][A-Za-z0-9_-]{0,40}$/
const ATTR_RE = /^[A-Za-z_:][A-Za-z0-9_:-]{0,40}$/

export function parseExtract(
  raw: unknown
): { ok: true; extract?: CliExtract } | { ok: false; error: string } {
  if (raw == null) return { ok: true }
  if (typeof raw !== 'object' || Array.isArray(raw)) return { ok: false, error: 'extract must be an object' }
  const row = raw as Record<string, unknown>
  if (typeof row.selector !== 'string' || !row.selector.trim()) {
    return { ok: false, error: 'extract.selector is required' }
  }
  const selector = row.selector.trim()
  if (selector.length > 400 || /[\u0000-\u001f]/.test(selector)) {
    return { ok: false, error: 'extract.selector is invalid' }
  }
  let limit = 5
  if (row.limit != null) {
    if (typeof row.limit !== 'number' || !Number.isInteger(row.limit) || row.limit < 1 || row.limit > 20) {
      return { ok: false, error: 'extract.limit must be an integer from 1 to 20' }
    }
    limit = row.limit
  }
  if (!Array.isArray(row.fields) || row.fields.length < 1 || row.fields.length > 6) {
    return { ok: false, error: 'extract.fields needs 1 to 6 fields' }
  }
  const fields: CliExtractField[] = []
  const names = new Set<string>()
  for (const field of row.fields) {
    if (!field || typeof field !== 'object' || Array.isArray(field)) {
      return { ok: false, error: 'extract field must be an object' }
    }
    const item = field as Record<string, unknown>
    if (typeof item.name !== 'string' || !FIELD_NAME_RE.test(item.name)) {
      return { ok: false, error: 'extract field name must be a short identifier' }
    }
    if (names.has(item.name)) return { ok: false, error: `duplicate extract field: ${item.name}` }
    names.add(item.name)
    const next: CliExtractField = { name: item.name }
    if (item.selector != null) {
      if (typeof item.selector !== 'string' || !item.selector.trim() || item.selector.length > 200) {
        return { ok: false, error: `extract field ${item.name} has a bad selector` }
      }
      if (/[\u0000-\u001f]/.test(item.selector)) {
        return { ok: false, error: `extract field ${item.name} has a bad selector` }
      }
      next.selector = item.selector.trim()
    }
    if (item.attr != null && item.attr !== 'text') {
      if (typeof item.attr !== 'string' || !ATTR_RE.test(item.attr)) {
        return { ok: false, error: `extract field ${item.name} has a bad attr` }
      }
      next.attr = item.attr
    }
    fields.push(next)
  }
  return { ok: true, extract: { selector, limit, fields } }
}

/** Keep a valid extract and drop a broken one. Other command fields stay as saved. */
export function attachExtract(command: CliCommand): CliCommand {
  const parsed = parseExtract(command.extract)
  if (parsed.ok && parsed.extract) return { ...command, extract: parsed.extract }
  if (command.extract == null) return command
  const next = { ...command }
  delete next.extract
  return next
}

export function scrubExtractItems(
  items: Array<Record<string, string>>,
  fields: CliExtractField[] = []
): Array<Record<string, string>> {
  return items.map((item) => {
    const out: Record<string, string> = {}
    const write = (key: string, value: string): void => {
      const text = secretField(key) ? '****' : scrubSecrets(value)
      out[key] = text.length > FIELD_VALUE_MAX ? text.slice(0, FIELD_VALUE_MAX) : text
    }
    const seen = new Set<string>()
    for (const field of fields) {
      if (!Object.prototype.hasOwnProperty.call(item, field.name)) continue
      write(field.name, item[field.name] ?? '')
      seen.add(field.name)
    }
    for (const [key, value] of Object.entries(item)) {
      if (seen.has(key)) continue
      write(key, value)
    }
    return out
  })
}

function looksLikeHtml(body: string): boolean {
  const head = body.slice(0, 300).trimStart().toLowerCase()
  return (
    head.startsWith('<!doctype html') ||
    head.startsWith('<html') ||
    head.startsWith('<head') ||
    head.startsWith('<body')
  )
}

/** HTML pages are previewed. JSON and other short bodies stay whole. */
/** The run body to show in a JSON viewer, or null when it is cut, extracted, or not JSON. */
export function runJsonBody(result: {
  status?: number
  body?: string
  bodyCut?: boolean
  items?: Array<Record<string, string>>
}): string | null {
  if (result.items || result.bodyCut || !result.body) return null
  const trimmed = result.body.trim()
  if (!(trimmed.startsWith('{') || trimmed.startsWith('['))) return null
  try {
    JSON.parse(trimmed)
    return result.body
  } catch {
    return null
  }
}

export function presentRawBody(body: string, bodyTruncated: boolean): { body: string; bodyCut: boolean } {
  if (!looksLikeHtml(body)) return { body, bodyCut: false }
  if (!bodyTruncated && body.length <= RAW_PREVIEW_CHARS) return { body, bodyCut: false }
  return { body: body.slice(0, RAW_PREVIEW_CHARS), bodyCut: true }
}

/** In-page fetch plus DOMParser. Selector text is JSON, never spliced into the script. */
export function buildExtractExpression(spec: {
  url: string
  method: string
  headers: Record<string, string>
  body?: string
  extract: CliExtract
}): string {
  const hasBody = spec.body !== undefined && spec.method !== 'GET' && spec.method !== 'HEAD'
  const job = {
    url: spec.url,
    method: spec.method,
    headers: spec.headers,
    ...(hasBody ? { body: spec.body } : {}),
    timeoutMs: EXTRACT_FETCH_TIMEOUT_MS,
    maxChars: EXTRACT_MAX_CHARS,
    selector: spec.extract.selector,
    limit: spec.extract.limit,
    fields: spec.extract.fields,
    noteNone: EXTRACT_NOTE_NONE,
    noteCapped: EXTRACT_NOTE_CAPPED
  }
  return `
(async () => {
  const job = ${JSON.stringify(job)}
  const ac = new AbortController()
  const timer = setTimeout(() => ac.abort(), job.timeoutMs)
  try {
    const init = {
      method: job.method,
      headers: job.headers,
      credentials: 'include',
      redirect: 'follow',
      cache: 'no-store',
      signal: ac.signal
    }
    if (job.body != null && job.method !== 'GET' && job.method !== 'HEAD') init.body = job.body
    const res = await fetch(job.url, init)
    clearTimeout(timer)
    const buf = await res.arrayBuffer()
    const bytes = new Uint8Array(buf)
    const total = bytes.length
    const slice = total > job.maxChars ? bytes.slice(0, job.maxChars) : bytes
    const ctm = /charset\\s*=\\s*["']?([^;"'\\s]+)/i.exec(res.headers.get('content-type') || '')
    let html
    try {
      html = new TextDecoder(ctm ? ctm[1].toLowerCase() : 'utf-8', { fatal: false }).decode(slice)
    } catch (_e) {
      html = new TextDecoder('utf-8', { fatal: false }).decode(slice)
    }
    if (html.charCodeAt(0) === 0xfeff) html = html.slice(1)
    const doc = new DOMParser().parseFromString(html, 'text/html')
    let nodes
    try {
      nodes = Array.from(doc.querySelectorAll(job.selector))
    } catch (_e) {
      return { status: res.status, error: 'invalid selector', items: [], matched: 0 }
    }
    const items = []
    for (const node of nodes.slice(0, job.limit)) {
      const item = {}
      for (const field of job.fields) {
        let el = node
        if (field.selector) {
          try { el = node.querySelector(field.selector) } catch (_e) { el = null }
        }
        let value = ''
        if (el) {
          if (field.attr && field.attr !== 'text') value = el.getAttribute(field.attr) || ''
          else value = el.textContent || ''
        }
        value = value.replace(/\\s+/g, ' ').trim()
        if (value.length > 500) value = value.slice(0, 500)
        if ((field.attr === 'href' || field.attr === 'src') && value) {
          try { value = new URL(value, res.url).href } catch (_e) {}
        }
        item[field.name] = value
      }
      items.push(item)
    }
    const truncated = total > job.maxChars
    let note
    if (nodes.length === 0) note = truncated ? job.noteNone + '; ' + job.noteCapped : job.noteNone
    else if (truncated) note = job.noteCapped
    return { status: res.status, matched: nodes.length, items, note }
  } catch (e) {
    clearTimeout(timer)
    const isAbort = e && e.name === 'AbortError'
    return {
      status: 0,
      items: [],
      matched: 0,
      error: isAbort
        ? 'fetch aborted after ' + job.timeoutMs + 'ms with no response'
        : ((e && e.message) ? e.message : String(e))
    }
  }
})()
`
}
