import { mask } from './secret-scan'

// Neutral intermediate form for a captured request, shared by every language
// emitter (python / curl / typescript) and reused by recipe/openapi codegen.

export type BodyType = 'json' | 'form' | 'text' | 'none'

export interface RequestIR {
  method: string
  url: string
  headers: Record<string, string>
  cookies: Record<string, string>
  body?: string
  bodyType: BodyType
}

export interface CodegenInput {
  method: string
  url: string
  requestHeaders?: Record<string, string>
  requestPostData?: string
}

export type CodegenLang = 'python' | 'curl' | 'typescript'

export interface CodegenOptions {
  lang: CodegenLang
  maskSecrets?: boolean
  pythonLibrary?: 'requests' | 'httpx'
}

// Header names whose values are credentials and should be masked by default.
const SENSITIVE_HEADER_RE =
  /^(authorization|cookie|proxy-authorization|x-api-key|api-key|x-auth-token|x-access-token|x-csrf-token|x-xsrf-token)$/i
// Cookie / query-param / body-field names that look like credentials or
// session material.
const SENSITIVE_FIELD_RE = /(token|session|sid|auth|secret|key|jwt|csrf|xsrf|pass(word|wd)?|pwd|credential)/i

function headerValue(headers: Record<string, string>, name: string): string | undefined {
  const lc = name.toLowerCase()
  for (const [k, v] of Object.entries(headers)) if (k.toLowerCase() === lc) return v
  return undefined
}

function detectBodyType(headers: Record<string, string>, body: string | undefined): BodyType {
  if (body == null) return 'none'
  const ct = (headerValue(headers, 'content-type') ?? '').toLowerCase()
  if (ct.includes('json')) return 'json'
  if (ct.includes('x-www-form-urlencoded')) return 'form'
  return 'text'
}

export function buildRequestIR(input: CodegenInput): RequestIR {
  const headers: Record<string, string> = {}
  let cookieHeader = ''
  for (const [k, v] of Object.entries(input.requestHeaders ?? {})) {
    if (k.toLowerCase() === 'cookie') {
      cookieHeader = v
      continue
    }
    headers[k] = v
  }
  const cookies: Record<string, string> = {}
  if (cookieHeader) {
    for (const part of cookieHeader.split(';')) {
      const idx = part.indexOf('=')
      if (idx === -1) continue
      cookies[part.slice(0, idx).trim()] = part.slice(idx + 1).trim()
    }
  }
  return {
    method: (input.method || 'GET').toUpperCase(),
    url: input.url,
    headers,
    cookies,
    body: input.requestPostData ?? undefined,
    bodyType: detectBodyType(headers, input.requestPostData ?? undefined)
  }
}

function maskRecord(rec: Record<string, string>, re: RegExp): Record<string, string> {
  const out: Record<string, string> = {}
  for (const [k, v] of Object.entries(rec)) out[k] = re.test(k) ? mask(v) : v
  return out
}

function maskUrl(url: string): string {
  let u: URL
  try {
    u = new URL(url)
  } catch {
    return url
  }
  let changed = false
  for (const [k, v] of [...u.searchParams]) {
    if (!SENSITIVE_FIELD_RE.test(k)) continue
    u.searchParams.set(k, mask(v))
    changed = true
  }
  return changed ? u.toString() : url
}

function maskJsonValue(v: unknown): { value: unknown; changed: boolean } {
  if (Array.isArray(v)) {
    let changed = false
    const value = v.map((x) => {
      const r = maskJsonValue(x)
      changed ||= r.changed
      return r.value
    })
    return { value, changed }
  }
  if (v && typeof v === 'object') {
    let changed = false
    const value: Record<string, unknown> = {}
    for (const [k, x] of Object.entries(v)) {
      if (SENSITIVE_FIELD_RE.test(k) && typeof x === 'string') {
        value[k] = mask(x)
        changed = true
      } else {
        const r = maskJsonValue(x)
        changed ||= r.changed
        value[k] = r.value
      }
    }
    return { value, changed }
  }
  return { value: v, changed: false }
}

function maskBody(ir: RequestIR): string | undefined {
  if (ir.body == null) return ir.body
  if (ir.bodyType === 'json') {
    try {
      const r = maskJsonValue(JSON.parse(ir.body))
      // Re-serialize only when something was masked, so untouched bodies keep
      // their exact bytes.
      return r.changed ? JSON.stringify(r.value) : ir.body
    } catch {
      return ir.body
    }
  }
  if (ir.bodyType === 'form') {
    const fields = parseFormBody(ir.body)
    if (!Object.keys(fields).some((k) => SENSITIVE_FIELD_RE.test(k))) return ir.body
    return Object.entries(maskRecord(fields, SENSITIVE_FIELD_RE))
      .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`)
      .join('&')
  }
  return ir.body
}

// Credential-bearing parts of the request (headers, cookies, query params,
// JSON/form body fields) with their values masked. Plain-text bodies are left
// as-is — there's no structure to find a secret in.
function applyMask(ir: RequestIR, on: boolean): RequestIR {
  if (!on) return ir
  return {
    ...ir,
    url: maskUrl(ir.url),
    headers: maskRecord(ir.headers, SENSITIVE_HEADER_RE),
    cookies: maskRecord(ir.cookies, SENSITIVE_FIELD_RE),
    body: maskBody(ir)
  }
}

function parseFormBody(body: string): Record<string, string> {
  const out: Record<string, string> = {}
  for (const part of body.split('&')) {
    if (!part) continue
    const idx = part.indexOf('=')
    const k = idx === -1 ? part : part.slice(0, idx)
    const v = idx === -1 ? '' : part.slice(idx + 1)
    try {
      out[decodeURIComponent(k)] = decodeURIComponent(v)
    } catch {
      out[k] = v
    }
  }
  return out
}

// A JSON object of string values is already a valid Python dict literal.
function pyDict(v: Record<string, string>): string {
  return JSON.stringify(v, null, 4)
}

// POSIX single-quoting: nothing inside is expanded, so captured values like
// `$(…)` or backticks stay literal when the user runs the command.
function shq(v: string): string {
  return `'${v.replace(/'/g, `'\\''`)}'`
}

export function emitPython(raw: RequestIR, opts: { maskSecrets: boolean; library: 'requests' | 'httpx' }): string {
  const lib = opts.library === 'httpx' ? 'httpx' : 'requests'
  const clientClass = lib === 'httpx' ? 'httpx.Client' : 'requests.Session'
  const ir = applyMask(raw, opts.maskSecrets)
  const { headers, cookies } = ir
  const method = ir.method.toLowerCase()
  const lines: string[] = [`import ${lib}`, '', `s = ${clientClass}()`]
  if (Object.keys(headers).length) lines.push(`s.headers.update(${pyDict(headers)})`)
  if (Object.keys(cookies).length) lines.push(`s.cookies.update(${pyDict(cookies)})`)

  let call = `resp = s.${method}(${JSON.stringify(ir.url)}`
  if (ir.bodyType === 'json' && ir.body != null) {
    // Send the exact JSON bytes; Content-Type comes from the captured headers.
    lines.push(`data = ${JSON.stringify(ir.body)}`)
    call += `, data=data`
  } else if (ir.bodyType === 'form' && ir.body != null) {
    lines.push(`data = ${pyDict(parseFormBody(ir.body))}`)
    call += `, data=data`
  } else if (ir.body != null) {
    lines.push(`data = ${JSON.stringify(ir.body)}`)
    call += `, data=data`
  }
  call += ')'
  lines.push(call, 'print(resp.status_code, resp.text[:500])')
  return lines.join('\n')
}

export function emitCurl(raw: RequestIR, opts: { maskSecrets: boolean }): string {
  const ir = applyMask(raw, opts.maskSecrets)
  const { headers, cookies } = ir
  const method = /^[A-Z]+$/.test(ir.method) ? ir.method : shq(ir.method)
  const parts: string[] = [`curl -X ${method} ${shq(ir.url)}`]
  for (const [k, v] of Object.entries(headers)) parts.push(`  -H ${shq(`${k}: ${v}`)}`)
  const cookieStr = Object.entries(cookies)
    .map(([k, v]) => `${k}=${v}`)
    .join('; ')
  if (cookieStr) parts.push(`  -b ${shq(cookieStr)}`)
  // --data-raw: plain --data would read a file when the body starts with "@".
  if (ir.body != null) parts.push(`  --data-raw ${shq(ir.body)}`)
  return parts.join(' \\\n')
}

export function emitTypeScript(raw: RequestIR, opts: { maskSecrets: boolean }): string {
  const ir = applyMask(raw, opts.maskSecrets)
  const headers = { ...ir.headers }
  const cookieStr = Object.entries(ir.cookies)
    .map(([k, v]) => `${k}=${v}`)
    .join('; ')
  if (cookieStr) headers['Cookie'] = cookieStr
  const lines: string[] = [
    `const res = await fetch(${JSON.stringify(ir.url)}, {`,
    `  method: ${JSON.stringify(ir.method)},`,
    `  headers: ${JSON.stringify(headers, null, 2).replace(/\n/g, '\n  ')},`,
    `  credentials: 'include'${ir.body != null ? ',' : ''}`
  ]
  if (ir.body != null) lines.push(`  body: ${JSON.stringify(ir.body)}`)
  lines.push('})')
  lines.push('console.log(res.status, await res.text())')
  return lines.join('\n')
}

export function generateCode(input: CodegenInput, opts: CodegenOptions): string {
  const ir = buildRequestIR(input)
  const maskSecrets = opts.maskSecrets ?? true
  switch (opts.lang) {
    case 'curl':
      return emitCurl(ir, { maskSecrets })
    case 'typescript':
      return emitTypeScript(ir, { maskSecrets })
    case 'python':
    default:
      return emitPython(ir, { maskSecrets, library: opts.pythonLibrary ?? 'requests' })
  }
}
