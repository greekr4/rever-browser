import { spawn } from 'node:child_process'

import { listRequests, type StoredRequest } from '../traffic-store'

export interface ScriptFilter {
  host?: string
  limit?: number
  minSize?: number
}

export function listScripts(filter: ScriptFilter = {}): StoredRequest[] {
  const rows = listRequests({
    methodOrType: 'Script',
    host: filter.host,
    limit: Math.max(filter.limit ?? 50, 50)
  })
  const minSize = filter.minSize ?? 0
  const filtered = rows.filter(
    (r) =>
      !!r.responseBody &&
      !r.responseBodyBase64 &&
      (r.encodedDataLength ?? r.responseBody!.length) >= minSize
  )
  filtered.sort((a, b) => (b.responseBody?.length ?? 0) - (a.responseBody?.length ?? 0))
  return filtered.slice(0, filter.limit ?? 50)
}

export type Category =
  | 'api'
  | 'urls'
  | 'env'
  | 'secrets'
  | 'auth'
  | 'hooks'
  | 'fetch'
  | 'ai'
  | 'baas'
  | 'function-calling'
  | 'rpc'

const CATEGORY_PATTERNS: Record<Category, RegExp> = {
  api: /["'`](\/api\/[^"'`]+|[^"'`]*\/(?:v1|v2|v3)\/[^"'`]+)["'`]/g,
  urls: /https?:\/\/[a-zA-Z0-9._:/?#@!$&'()*+,;=~%-]+/g,
  env: /NEXT_PUBLIC_[A-Z0-9_]+|process\.env\.[A-Z0-9_]+|VITE_[A-Z0-9_]+/g,
  secrets: /eyJ[a-zA-Z0-9_-]{20,}|sb_publishable_[a-zA-Z0-9_-]+|AIza[a-zA-Z0-9_-]{30,}/g,
  auth: /accessToken|refreshToken|signIn|signOut|signUp|getSession|supabase|firebase|clerk/g,
  hooks: /\buse[A-Z][a-zA-Z]+\b/g,
  fetch: /fetch\([^)]{5,120}\)|axios\.[a-z]+\(/g,
  ai: /gemini|gpt-[34]|claude-?[a-z0-9]*|anthropic|openai|systemInstruction|systemPrompt/gi,
  baas: /[a-z0-9-]+\.supabase\.co|[a-z0-9-]+\.firebaseapp\.com|[a-z0-9-]+\.firebaseio\.com/g,
  'function-calling': /name:\s*["'][a-z_][a-z0-9_]*["']\s*,\s*description:\s*["'][^"']{5,}["']/g,
  rpc: /this\.client\.(?:request|stream)\(["'][^"']+["']/g
}

const URL_NOISE = /(w3\.org|schema\.org|mozilla\.org|fonts\.googleapis|fonts\.gstatic|github\.com\/[^"]*\/issues|webpack|reactjs\.org|nextjs\.org)/

export function patternForCategory(c: Category): RegExp {
  return CATEGORY_PATTERNS[c]
}

export interface GrepMatch {
  offset: number
  match: string
  snippet: string
}

export interface GrepOptions {
  max: number
  before: number
  after: number
}

// grepBody 본문 크기 상한: 10MB
const GREP_BODY_MAX_BYTES = 10 * 1024 * 1024
// 단일 grepBody 호출당 최대 exec 시도 횟수 (ReDoS 방어)
const GREP_MAX_EXEC_ATTEMPTS = 10_000

export function grepBody(body: string, regex: RegExp, opts: GrepOptions): GrepMatch[] {
  // 본문 크기 상한 — 초과 시 빈 결과 반환
  if (body.length > GREP_BODY_MAX_BYTES) {
    return []
  }
  const out: GrepMatch[] = []
  const flags = regex.flags.includes('g') ? regex.flags : regex.flags + 'g'
  const re = new RegExp(regex.source, flags)
  const seen = new Set<string>()
  let m: RegExpExecArray | null
  let attempts = 0
  while ((m = re.exec(body)) && out.length < opts.max) {
    if (++attempts > GREP_MAX_EXEC_ATTEMPTS) break
    if (m[0].length === 0) {
      re.lastIndex++
      continue
    }
    if (regex === CATEGORY_PATTERNS.urls && URL_NOISE.test(m[0])) continue
    const key = `${m[0]}@${Math.floor(m.index / 200)}`
    if (seen.has(key)) continue
    seen.add(key)
    const start = Math.max(0, m.index - opts.before)
    const end = Math.min(body.length, m.index + m[0].length + opts.after)
    out.push({ offset: m.index, match: m[0], snippet: body.slice(start, end) })
  }
  return out
}

export function detectBundler(body: string): { name: string; signature: string } {
  const head = body.slice(0, 8000)
  const tail = body.slice(-4000)
  const both = head + tail
  if (/__vite__|import\.meta\.(?:url|env|hot)/.test(both))
    return { name: 'vite', signature: 'import.meta / __vite__' }
  if (/\bturbopack\b/i.test(both)) return { name: 'turbopack', signature: 'turbopack marker' }
  if (/__webpack_require__|webpackJsonp|webpackChunk/.test(both))
    return { name: 'webpack', signature: '__webpack_require__ / webpackJsonp' }
  if (/require=function\s*\(/.test(head) && /browserify/i.test(both))
    return { name: 'browserify', signature: 'browserify runtime' }
  if (/Object\.defineProperty\(exports/.test(head) && /import\s*\{[^}]+\}\s*from\s*["']/.test(both))
    return { name: 'rollup', signature: 'rollup ESM output' }
  return { name: 'unknown', signature: '' }
}

const WEBCRACK_TIMEOUT_MS = 30_000
const WEBCRACK_MAX_OUTPUT = 5 * 1024 * 1024

export function runWebcrack(body: string): Promise<string> {
  return new Promise((resolve, reject) => {
    let proc: ReturnType<typeof spawn>
    try {
      proc = spawn('webcrack', ['-'], { stdio: ['pipe', 'pipe', 'pipe'] })
    } catch (e) {
      reject(e)
      return
    }

    const chunks: Buffer[] = []
    let total = 0
    let killed = false

    const timer = setTimeout(() => {
      killed = true
      proc.kill('SIGKILL')
    }, WEBCRACK_TIMEOUT_MS)

    proc.on('error', (e: NodeJS.ErrnoException) => {
      clearTimeout(timer)
      if (e.code === 'ENOENT') {
        reject(
          new Error(
            'webcrack CLI not found on PATH. Install with: nvm use 22 && npm i -g webcrack'
          )
        )
      } else {
        reject(e)
      }
    })

    proc.stdout!.on('data', (c: Buffer) => {
      total += c.length
      if (total > WEBCRACK_MAX_OUTPUT) {
        killed = true
        proc.kill('SIGKILL')
        return
      }
      chunks.push(c)
    })

    proc.stderr!.on('data', () => {})

    proc.on('close', (code) => {
      clearTimeout(timer)
      if (killed) {
        reject(new Error(`webcrack timed out or output exceeded ${WEBCRACK_MAX_OUTPUT} bytes`))
        return
      }
      if (code !== 0 && chunks.length === 0) {
        reject(new Error(`webcrack exited with code ${code}`))
        return
      }
      resolve(Buffer.concat(chunks).toString('utf8'))
    })

    proc.stdin!.end(body)
  })
}
