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
// Cookie names that look like credentials/session material.
const SENSITIVE_COOKIE_RE = /(token|session|sid|auth|secret|key|jwt|csrf|xsrf)/i

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

function maskHeaders(ir: RequestIR, on: boolean): Record<string, string> {
  if (!on) return ir.headers
  const out: Record<string, string> = {}
  for (const [k, v] of Object.entries(ir.headers)) {
    out[k] = SENSITIVE_HEADER_RE.test(k) ? mask(v) : v
  }
  return out
}

function maskCookies(ir: RequestIR, on: boolean): Record<string, string> {
  if (!on) return ir.cookies
  const out: Record<string, string> = {}
  for (const [k, v] of Object.entries(ir.cookies)) {
    out[k] = SENSITIVE_COOKIE_RE.test(k) ? mask(v) : v
  }
  return out
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

function py(v: unknown): string {
  // JSON is close enough to a Python literal for str/number/array/object, but
  // booleans/null differ. json.dumps-style via JSON then fix tokens.
  return JSON.stringify(v, null, 4)
    .replace(/: true\b/g, ': True')
    .replace(/: false\b/g, ': False')
    .replace(/: null\b/g, ': None')
}

export function emitPython(ir: RequestIR, opts: { maskSecrets: boolean; library: 'requests' | 'httpx' }): string {
  const lib = opts.library === 'httpx' ? 'httpx' : 'requests'
  const clientClass = lib === 'httpx' ? 'httpx.Client' : 'requests.Session'
  const headers = maskHeaders(ir, opts.maskSecrets)
  const cookies = maskCookies(ir, opts.maskSecrets)
  const method = ir.method.toLowerCase()
  const lines: string[] = [`import ${lib}`, '', `s = ${clientClass}()`]
  if (Object.keys(headers).length) lines.push(`s.headers.update(${py(headers)})`)
  if (Object.keys(cookies).length) lines.push(`s.cookies.update(${py(cookies)})`)

  let call = `resp = s.${method}(${JSON.stringify(ir.url)}`
  if (ir.bodyType === 'json' && ir.body != null) {
    // Send the exact JSON bytes; Content-Type comes from headers. (Fixes the
    // old generator, which sent JSON bodies through form-style data=.)
    lines.push(`data = ${JSON.stringify(ir.body)}`)
    call += `, data=data`
  } else if (ir.bodyType === 'form' && ir.body != null) {
    lines.push(`data = ${py(parseFormBody(ir.body))}`)
    call += `, data=data`
  } else if (ir.body != null) {
    lines.push(`data = ${JSON.stringify(ir.body)}`)
    call += `, data=data`
  }
  call += ')'
  lines.push(call, 'print(resp.status_code, resp.text[:500])')
  return lines.join('\n')
}

export function emitCurl(ir: RequestIR, opts: { maskSecrets: boolean }): string {
  const headers = maskHeaders(ir, opts.maskSecrets)
  const cookies = maskCookies(ir, opts.maskSecrets)
  const parts: string[] = [`curl -X ${ir.method} ${JSON.stringify(ir.url)}`]
  for (const [k, v] of Object.entries(headers)) parts.push(`  -H ${JSON.stringify(`${k}: ${v}`)}`)
  const cookieStr = Object.entries(cookies)
    .map(([k, v]) => `${k}=${v}`)
    .join('; ')
  if (cookieStr) parts.push(`  -b ${JSON.stringify(cookieStr)}`)
  if (ir.body != null) parts.push(`  --data ${JSON.stringify(ir.body)}`)
  return parts.join(' \\\n')
}

export function emitTypeScript(ir: RequestIR, opts: { maskSecrets: boolean }): string {
  const headers = { ...maskHeaders(ir, opts.maskSecrets) }
  const cookieStr = Object.entries(maskCookies(ir, opts.maskSecrets))
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
