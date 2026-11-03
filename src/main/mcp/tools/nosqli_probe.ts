import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { buildRequestSpec, repeaterSendRaw, restoreMarker } from '../../repeater'
import { ok, err, errorMessage } from '../utils'

// NoSQL / Cypher injection probe — the sqli_probe pattern for Mongo/Neo4j stacks
// where sqli_probe is blind (Mongo throws no SQL error, ignores `' OR 2*3=6`).
// Two signals:
//   - operator differential: the param is rewritten to a Mongo operator clause,
//     once broadly-matching (`param[$ne]=<nonce>`) and once matching-nothing
//     (`param[$eq]=<nonce>`). If the two responses differ (after stripping the
//     nonce echo), the operator was interpreted = the param reaches a query as
//     an object, not a string — the `$ne` auth-bypass / filter-bypass class.
//   - error signature: a syntax-breaking value surfaces a Neo4j/Cypher/Mongo
//     parse error in the body, proving the value reaches a query engine unsafely.

export type NosqliContext = 'op-true' | 'op-false' | 'error'

export interface NosqliCase {
  context: NosqliContext
  op?: 'ne' | 'eq'
  value: string
}

function randomNonce(): string {
  return 'z' + Math.random().toString(36).slice(2, 7) + 'z'
}

export function buildNosqliCases(nonce = randomNonce()): NosqliCase[] {
  return [
    { context: 'op-true', op: 'ne', value: nonce },
    { context: 'op-false', op: 'eq', value: nonce },
    // A breaker that upsets Mongo `$where`/JSON parsing and Cypher alike.
    { context: 'error', value: `${nonce}'"\\)}];//` }
  ]
}

/** NoSQL/Cypher driver error signatures that leak into a response body. */
export const NOSQL_ERROR_RE =
  /(neo4j|cypher|Neo\.ClientError|Neo\.DatabaseError|mongoerror|mongoservererror|mongoparseerror|bsonerror|casterror|E11000|\$where|unexpected token.*json|invalid regular expression|unterminated)/i

export interface NosqliReply {
  context: NosqliContext
  status: number
  body: string
  nonce: string
}

export interface NosqliOutcome {
  nosqli: boolean
  contexts: string[]
}

function normalizeBody(r: NosqliReply): string {
  const stripped = r.body.split(r.nonce).join('')
  return `${r.status}|${stripped.replace(/\s+/g, ' ')}`
}

/** Confirmed iff a NoSQL error signature appears, or the operator pair differs. */
export function nosqliEvaluated(replies: NosqliReply[]): NosqliOutcome {
  const byCtx = new Map(replies.map((r) => [r.context, r]))
  const contexts: string[] = []
  const e = byCtx.get('error')
  if (e && NOSQL_ERROR_RE.test(e.body)) contexts.push('error')
  const t = byCtx.get('op-true')
  const f = byCtx.get('op-false')
  if (t && f && normalizeBody(t) !== normalizeBody(f)) contexts.push('operator')
  return { nosqli: contexts.length > 0, contexts }
}

/** Rewrite a `key=val` grammar string (query or form body), replacing the
 *  target param's pair with `replacement`, or appending it if absent. */
export function injectKv(s: string, param: string, replacement: string): string {
  const esc = param.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const re = new RegExp(`(^|&)${esc}(\\[[^\\]]*\\])?=[^&]*`)
  if (re.test(s)) return s.replace(re, (_m, p1) => `${p1}${replacement}`)
  return s ? `${s}&${replacement}` : replacement
}

/** Set one key of a JSON object body to `value` (object or scalar) and reserialize. */
export function injectJson(body: string, param: string, value: unknown): string {
  const obj = JSON.parse(body || '{}')
  obj[param] = value
  return JSON.stringify(obj)
}

export function registerNosqliProbeTools(mcp: McpServer) {
  mcp.registerTool(
    'nosqli_probe',
    {
      description:
        'Confirm NoSQL (MongoDB) / Cypher (Neo4j) injection at a request parameter — use this, not sqli_probe, when the backend is Mongo/Neo4j (sqli_probe is SQL-error based and blind to them). Fires an operator differential (`param[$ne]=<nonce>` vs `param[$eq]=<nonce>`: the responses differ only if the param reaches a query as an object — the `$ne` auth/filter-bypass class) plus a syntax-breaker that surfaces a Neo4j/Cypher/Mongo parse error. Pass param=<field name>; the tool rewrites form, JSON, or query carriers itself. Login/search fields on a Mongo/Neo4j app are the classic carriers. A positive is auth-bypass / data-exfiltration class; record as critical.',
      inputSchema: {
        requestId: z.string(),
        param: z.string().describe('Field name that carries the value (e.g. "username") — the tool rewrites it for Mongo/Cypher injection'),
        nonce: z.string().optional().describe('Override the random nonce (test determinism)')
      }
    },
    async ({ requestId, param, nonce }) => {
      try {
        const base = restoreMarker(buildRequestSpec(requestId, undefined))
        const inQuery = base.url.includes(`${encodeURIComponent(param)}=`) || base.url.includes(`${param}=`)
        const ctype = Object.entries(base.headers).find(([k]) => k.toLowerCase() === 'content-type')?.[1] ?? ''
        const isJson = /application\/json/i.test(ctype)
        if (!inQuery && !base.body) {
          return err(`no carrier for "${param}": the request has no query string and no body to inject into`)
        }

        const cases = buildNosqliCases(nonce)
        const replies: NosqliReply[] = []
        const results: Array<{ context: string; status: number; preview: string }> = []
        for (const c of cases) {
          const sub: { url: string; method: string; headers: Record<string, string>; body?: string } = {
            url: base.url,
            method: base.method,
            headers: { ...base.headers },
            body: base.body
          }
          const kvReplacement = c.op ? `${param}[$${c.op}]=${c.value}` : `${param}=${encodeURIComponent(c.value)}`
          if (inQuery) {
            const [path, qs = ''] = sub.url.split('?')
            sub.url = `${path}?${injectKv(qs, param, kvReplacement)}`
          } else if (isJson) {
            sub.body = injectJson(sub.body ?? '', param, c.op ? { [`$${c.op}`]: c.value } : c.value)
          } else {
            sub.body = injectKv(sub.body ?? '', param, kvReplacement)
          }
          const r = await repeaterSendRaw(sub)
          replies.push({ context: c.context, status: r.status, body: r.body, nonce: c.value })
          const m = NOSQL_ERROR_RE.exec(r.body)
          results.push({
            context: c.context,
            status: r.status,
            preview: m ? r.body.slice(Math.max(0, m.index - 30), m.index + 60) : r.body.slice(0, 120)
          })
        }
        const { nosqli, contexts } = nosqliEvaluated(replies)
        return ok(
          JSON.stringify(
            {
              nosqli,
              contexts,
              note: nosqli
                ? 'NoSQL/Cypher injection confirmed (operator differential and/or driver error signature) — auth-bypass / data-exfiltration class, record as critical.'
                : 'No driver error and no operator differential; the param likely does not reach a Mongo/Neo4j query as an object. (A bcrypt-gated login compares the password outside the query, so a null result here is expected there.)',
              results
            },
            null,
            2
          )
        )
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
