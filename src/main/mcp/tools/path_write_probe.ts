import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { buildRequestSpec, repeaterSendRaw, restoreMarker } from '../../repeater'
import { ok, err, errorMessage } from '../utils'

// Arbitrary-file-write / path-traversal probe for a "stored then displayed"
// sink: a param whose value becomes a server-side FILENAME (e.g. Testimonial's
// `customer` → os.WriteFile("public/testimonials/"+customer)). Reflection
// probes (XSS/SSTI) miss it because the content is HTML-escaped on readback and
// the write itself is silent. We confirm it by a DIFFERENTIAL on the readback
// page: write a unique marker once with a plain filename, once with a `../`
// filename. If the marker shows up for the plain name but NOT for the traversal
// name, the `../` moved the write out of the served/read directory — i.e. the
// param is a raw path and traversal works (arbitrary file write).

function randomNonce(): string {
  return 'z' + Math.random().toString(36).slice(2, 7) + 'z'
}

export interface PathWritePlan {
  /** Unique marker written as the file CONTENT and searched for on readback. */
  marker1: string
  marker2: string
  /** Filename value for the tested param: plain, then traversal (one level up). */
  fname1: string
  fname2: string
}

export function buildPathWritePlan(nonce = randomNonce()): PathWritePlan {
  return {
    marker1: `rpw1${nonce}`,
    marker2: `rpw2${nonce}`,
    fname1: `rpwf1${nonce}`,
    fname2: `../rpwf2${nonce}`
  }
}

export type PathWriteKind = 'traversal' | 'stored-no-traversal' | 'inconclusive'

/**
 * plain marker seen on readback but traversal marker NOT → the `../` escaped the
 * served dir = path-traversal arbitrary write. Both seen → stored but traversal
 * did not move it (sanitised or same dir). Plain not seen → no readback hook, so
 * we cannot judge (the value may still be a filename elsewhere).
 */
export function pathWriteDecision(plainSeen: boolean, travSeen: boolean): { confirmed: boolean; kind: PathWriteKind } {
  if (plainSeen && !travSeen) return { confirmed: true, kind: 'traversal' }
  if (plainSeen && travSeen) return { confirmed: false, kind: 'stored-no-traversal' }
  return { confirmed: false, kind: 'inconclusive' }
}

/** Set the tested param to `value` and every OTHER param to `marker`, for both
 * query string and an x-www-form-urlencoded body. Returns the rewritten pair. */
export function placeMarkers(
  url: string,
  body: string | undefined,
  param: string,
  value: string,
  marker: string
): { url: string; body: string | undefined } {
  const rewriteQuery = (u: string): string => {
    try {
      const parsed = new URL(u)
      const keys = [...parsed.searchParams.keys()]
      for (const k of keys) parsed.searchParams.set(k, k === param ? value : marker)
      if (!keys.includes(param)) parsed.searchParams.set(param, value)
      return parsed.toString()
    } catch {
      return u
    }
  }
  const rewriteBody = (b: string): string => {
    const sp = new URLSearchParams(b)
    const keys = [...sp.keys()]
    for (const k of keys) sp.set(k, k === param ? value : marker)
    return sp.toString()
  }
  const urlHasParam = (() => {
    try {
      return new URL(url).searchParams.has(param)
    } catch {
      return false
    }
  })()
  // Put the tested param where it actually lives; if it's in neither, add it to
  // the query (the common stored-form case is GET like Testimonial).
  if (urlHasParam || !body) return { url: rewriteQuery(url), body: body ? rewriteBody(body) : body }
  return { url, body: rewriteBody(body) }
}

export function registerPathWriteProbeTools(mcp: McpServer) {
  mcp.registerTool(
    'path_write_probe',
    {
      description:
        'Confirm arbitrary file write / path traversal at a param whose value becomes a server-side FILENAME (stored-then-displayed sinks, e.g. a testimonial/comment/upload name). Writes a unique marker as the content twice — once with a plain filename, once with a `../` filename — then re-fetches the readback page: if the marker appears for the plain name but NOT for the traversal name, the `../` left the served directory, proving path traversal in the write path. Use this when a reflection probe says "stored but escaped (no XSS/SSTI)" on a multi-field form: the filename field is the real sink. A positive result is arbitrary-file-write class (→ overwrite source/templates = RCE); record as high/critical.',
      inputSchema: {
        requestId: z.string().describe('The captured write request (the form submit that stores content)'),
        param: z.string().describe('The param carrying the FILENAME (e.g. "customer"); other params are filled with the marker content'),
        readback: z
          .string()
          .optional()
          .describe('Absolute URL that displays stored content (default: the request URL without its query, i.e. the app root)')
      }
    },
    async ({ requestId, param, readback }) => {
      try {
        const base = restoreMarker(buildRequestSpec(requestId, undefined))
        const readbackUrl =
          readback ??
          (() => {
            try {
              const u = new URL(base.url)
              return `${u.origin}${u.pathname}`
            } catch {
              return base.url
            }
          })()

        const plan = buildPathWritePlan()
        const fetchReadback = async (): Promise<string> => {
          const r = await repeaterSendRaw({ url: readbackUrl, method: 'GET', headers: {}, body: undefined })
          return r.body
        }

        // 1) plain filename + marker1 content
        const w1 = placeMarkers(base.url, base.body, param, plan.fname1, plan.marker1)
        await repeaterSendRaw({ url: w1.url, method: base.method, headers: base.headers, body: w1.body })
        const plainSeen = (await fetchReadback()).includes(plan.marker1)

        // 2) traversal filename + marker2 content
        const w2 = placeMarkers(base.url, base.body, param, plan.fname2, plan.marker2)
        await repeaterSendRaw({ url: w2.url, method: base.method, headers: base.headers, body: w2.body })
        const travSeen = (await fetchReadback()).includes(plan.marker2)

        const { confirmed, kind } = pathWriteDecision(plainSeen, travSeen)
        const note =
          kind === 'traversal'
            ? 'Arbitrary file write via path traversal CONFIRMED: the plain-name marker rendered on readback but the `../` marker did not, so the filename param escapes the intended directory. Writing a source/template file (e.g. .templ/.html/.php) = RCE. Record high/critical.'
            : kind === 'stored-no-traversal'
              ? 'Content is stored and displayed, but `../` did not change where it landed (sanitised or single dir) — stored content injection, not traversal.'
              : 'Could not see the marker on the readback page — wrong readback URL, or the value is not displayed. Pass readback=<url that shows stored content> and retry.'
        return ok(
          JSON.stringify(
            { arbitraryWrite: confirmed, kind, plainSeen, travSeen, readback: readbackUrl, param, note },
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
