import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { buildRequestSpec, repeaterSendRaw, restoreMarker } from '../../repeater'
import { ok, err, errorMessage } from '../utils'

// Server-side template injection probe. Unlike payload_probe (which looks for the
// *literal* payload reflected back — the right test for XSS), SSTI is confirmed
// only when the template engine EVALUATES an expression: the payload text
// disappears and its computed result shows up instead. A single `{{7*7}}` proves
// nothing on a Java stack (Velocity/Freemarker ignore `{{}}`), so we fire one
// payload per common engine and look for the arithmetic result wrapped in a
// random nonce, which makes a coincidental match on the page essentially
// impossible.

export interface SstiCase {
  engine: string
  payload: string
  /** The string that appears in the body ONLY if the engine evaluated the payload. */
  expected: string
}

const PRIMES = [101, 103, 107, 109, 113, 127, 131, 137, 139, 149, 151, 157]

function randomNonce(): string {
  return 'z' + Math.random().toString(36).slice(2, 7) + 'z'
}

// Build one arithmetic payload per template engine. `a*b` is a product of two
// primes (distinctive, never a value like 49 that a page might print), wrapped in
// `nonce…nonce`. Evaluated output = `nonce{a*b}nonce` (= `expected`); un-evaluated
// output still contains the raw expression, which never matches `expected`.
export function buildSstiCases(
  a = PRIMES[Math.floor(Math.random() * PRIMES.length)],
  b = PRIMES[Math.floor(Math.random() * PRIMES.length)],
  nonce = randomNonce()
): SstiCase[] {
  const prod = a * b
  const expected = `${nonce}${prod}${nonce}`
  return [
    // Freemarker / JSP EL / Mako / generic `${…}`
    { engine: 'freemarker/jsp-el/generic ${}', payload: `${nonce}\${${a}*${b}}${nonce}`, expected },
    // Apache Velocity — needs #set; ${…} alone is a reference, not arithmetic
    { engine: 'velocity', payload: `#set($q=${a}*${b})${nonce}\${q}${nonce}`, expected },
    // Jinja2 / Twig / Nunjucks
    { engine: 'jinja2/twig', payload: `${nonce}{{${a}*${b}}}${nonce}`, expected },
    // ERB / EJS
    { engine: 'erb/ejs', payload: `${nonce}<%= ${a}*${b} %>${nonce}`, expected },
    // Thymeleaf inline
    { engine: 'thymeleaf', payload: `${nonce}[[\${${a}*${b}}]]${nonce}`, expected },
    // Razor
    { engine: 'razor', payload: `${nonce}@(${a}*${b})${nonce}`, expected }
  ]
}

/** A case evaluated iff the computed result (not the literal payload) is in the body. */
export function sstiEvaluated(body: string, c: SstiCase): boolean {
  return body.includes(c.expected)
}

export function registerSstiProbeTools(mcp: McpServer) {
  mcp.registerTool(
    'ssti_probe',
    {
      description:
        'Confirm server-side template injection at a request slot (marker §). Fires ONE arithmetic payload per template engine (Velocity, Freemarker/JSP-EL, Jinja2/Twig, ERB/EJS, Thymeleaf, Razor) and reports which engine EVALUATED it — i.e. the computed result (not the literal payload) appears in the response. Use this instead of concluding "not SSTI" from a single {{7*7}} probe: Java stacks only evaluate ${…}/#set, never {{…}}. A positive result is RCE-class (template eval → code execution); escalate severity accordingly.',
      inputSchema: {
        requestId: z.string(),
        a: z.number().int().optional().describe('First factor (test determinism; default random prime)'),
        b: z.number().int().optional().describe('Second factor (default random prime)')
      }
    },
    async ({ requestId, a, b }) => {
      try {
        const base = restoreMarker(buildRequestSpec(requestId, undefined))
        const markerIn = (s: string | undefined) => (s ?? '').includes('§')
        if (!markerIn(base.url) && !markerIn(base.body) && !Object.values(base.headers).some(markerIn)) {
          return err('marker § not found in request — mark the reflecting param (e.g. body text=§) first')
        }
        // A form-urlencoded body must carry the payload URL-encoded, or template
        // chars (= # & +) corrupt the field; other bodies (JSON) stay raw.
        const ctype = Object.entries(base.headers).find(([k]) => k.toLowerCase() === 'content-type')?.[1] ?? ''
        const formBody = /application\/x-www-form-urlencoded/i.test(ctype)

        const cases = buildSstiCases(a, b)
        const results: Array<{ engine: string; status: number; evaluated: boolean; preview: string }> = []
        const engines: string[] = []
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
          const evaluated = sstiEvaluated(r.body, c)
          if (evaluated) engines.push(c.engine)
          const idx = evaluated ? r.body.indexOf(c.expected) : -1
          results.push({
            engine: c.engine,
            status: r.status,
            evaluated,
            preview: idx >= 0 ? r.body.slice(Math.max(0, idx - 30), idx + c.expected.length + 30) : ''
          })
        }
        return ok(
          JSON.stringify(
            {
              ssti: engines.length > 0,
              engines,
              note:
                engines.length > 0
                  ? 'SSTI confirmed (template evaluated arithmetic) — RCE-class, record as critical.'
                  : 'No engine evaluated the payload; input is reflected literally (consider reflected XSS, not SSTI).',
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
