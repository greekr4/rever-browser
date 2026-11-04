import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { buildRequestSpec, restoreMarker } from '../../repeater'
import { isLoopbackHost, isLabHost } from '../fence'
import { getAltSession, setAltSession } from '../redteam-tenants'
import { ok, err, errorMessage } from '../utils'
import { buildRawRequest, parseRawResponse, sendRawOverSocket } from './raw_http'

// BOLA / IDOR probe — the one active test for broken object-level authorization,
// the dominant vuln class on UUID-keyed cookie-auth APIs (an ATS, a SaaS tenant
// backend) where injection probes don't apply. Confirming it needs a SECOND
// identity: it replays the owner's captured resource request three ways over a
// raw socket (fetch can't override the Cookie header — it's forbidden — so a raw
// socket is the only way to send another tenant's cookie):
//   - baseline: the owner's own cookie  → the resource (2xx).
//   - noauth:   no cookie               → must be denied / not the resource.
//   - cross:    the ALTERNATE tenant's cookie (from redteam_alt_session_set).
// BOLA is confirmed when the cross-tenant session reads the SAME resource that
// anonymous could not — the server keyed access on the object id alone, not on
// who owns it. Gated to loopback/lab, matching raw_http.

const DROP_HEADERS = new Set(['cookie', 'host', 'content-length', 'connection'])

/** Copy request headers, drop the ones buildRawRequest manages + any Cookie,
 *  then set Cookie to `cookie` (or leave it absent when null). */
export function withCookie(headers: Record<string, string>, cookie: string | null): Record<string, string> {
  const out: Record<string, string> = {}
  for (const [k, v] of Object.entries(headers)) {
    if (!DROP_HEADERS.has(k.toLowerCase())) out[k] = v
  }
  if (cookie) out['Cookie'] = cookie
  return out
}

/** The Cookie header value already on the captured request, if any. */
export function originalCookie(headers: Record<string, string>): string | null {
  const e = Object.entries(headers).find(([k]) => k.toLowerCase() === 'cookie')
  return e ? e[1] : null
}

/** A whitespace-stripped head of the body — enough to tell two JSON resources
 *  apart without being fooled by formatting. */
function fingerprint(b: string): string {
  return b.replace(/\s+/g, '').slice(0, 400)
}

/** Whether `b` returned the same distinctive resource as `a`. */
export function sameResource(a: string, b: string): boolean {
  if (!a || !b) return false
  const fa = fingerprint(a)
  if (fa.length < 20) return false // too small to be a distinctive resource
  if (fingerprint(b) === fa) return true
  return b.replace(/\s+/g, '').includes(fa.slice(0, 64))
}

export interface IdorReply {
  label: 'baseline' | 'noauth' | 'cross'
  status: number
  body: string
}

export interface IdorOutcome {
  bola: boolean
  note: string
}

export function idorDecision(baseline: IdorReply, noauth: IdorReply, cross: IdorReply): IdorOutcome {
  const ok2xx = (s: number): boolean => s >= 200 && s < 300
  if (!ok2xx(baseline.status)) {
    return { bola: false, note: `baseline ${baseline.status}: the resource isn't readable even as its owner — can't assess BOLA (bad requestId or expired owner cookie).` }
  }
  if (ok2xx(noauth.status) && sameResource(baseline.body, noauth.body)) {
    return { bola: false, note: `no-auth replay returned the same resource (${noauth.status}): it is readable with NO session at all — that's a public-exposure / broken-auth leak, not object-level auth. Record as info-disclosure.` }
  }
  if (ok2xx(cross.status) && sameResource(baseline.body, cross.body)) {
    return { bola: true, note: `the alternate tenant's session read the owner's resource (HTTP ${cross.status}, same body) while anonymous was denied (${noauth.status}) — the server authorizes on the object id alone. BOLA/IDOR confirmed (cross-tenant data access).` }
  }
  return { bola: false, note: `cross-tenant session got ${cross.status}${ok2xx(cross.status) ? ' with a different body' : ''}: the resource appears scoped to its owner.` }
}

export function registerIdorProbeTools(mcp: McpServer) {
  mcp.registerTool(
    'redteam_alt_session_set',
    {
      description:
        "Store a SECOND tenant's session Cookie for idor_probe to use as the cross-tenant attacker. Call this during auth bootstrap after registering + logging in a throwaway account: read its Cookie (cookie_list), then pass the full `Cookie:` header value here (e.g. \"ats_token=...; other=...\"). idor_probe then replays owner resources with this cookie to confirm cross-tenant access.",
      inputSchema: { cookie: z.string().describe('The full Cookie header value of the alternate (attacker) tenant session') }
    },
    async ({ cookie }) => {
      setAltSession(cookie)
      return ok('alternate-tenant session stored — idor_probe will use it as the cross-tenant attacker.')
    }
  )

  mcp.registerTool(
    'idor_probe',
    {
      description:
        'Confirm BOLA / IDOR (broken object-level authorization) at an authenticated resource — the active test for UUID/id-keyed cookie-auth APIs where injection probes do not apply. Replays the captured OWNER request three ways over a raw socket: owner cookie (baseline), no cookie (noauth), and the alternate tenant cookie set via redteam_alt_session_set (cross). Confirms BOLA when the cross-tenant session reads the same resource that anonymous could not. Needs an alternate-tenant session (set it first) and a captured authenticated request (its Cookie is the owner baseline; override with ownerCookie if the capture lacks it). Gated to loopback / lab hosts. A positive is cross-tenant data access — record as high/critical.',
      inputSchema: {
        requestId: z.string().describe('A captured authenticated request to a UUID/id-keyed resource (the owner baseline)'),
        altCookie: z.string().optional().describe('Alternate-tenant Cookie value (defaults to the one set via redteam_alt_session_set)'),
        ownerCookie: z.string().optional().describe('Owner Cookie value, if the captured request did not carry one')
      }
    },
    async ({ requestId, altCookie, ownerCookie }) => {
      try {
        const base = restoreMarker(buildRequestSpec(requestId, undefined))
        let u: URL
        try {
          u = new URL(base.url)
        } catch {
          return err(`captured request has no valid URL: ${base.url}`)
        }
        if (!isLoopbackHost(base.url) && !isLabHost(base.url)) {
          return err(`idor_probe sends raw sockets, gated to loopback / lab hosts; ${u.hostname} is neither. Mark it a lab target if you own it.`)
        }
        const alt = altCookie ?? getAltSession()
        if (!alt) {
          return err(
            'no alternate-tenant session: register a 2nd throwaway account, read its cookie (cookie_list), then redteam_alt_session_set({ cookie }) — or pass altCookie here.'
          )
        }
        const owner = ownerCookie ?? originalCookie(base.headers)
        if (!owner) {
          return err(
            'the captured request carries no Cookie header to use as the owner baseline — pass ownerCookie, or capture an authenticated request that includes the session cookie.'
          )
        }
        const host = u.hostname
        const port = Number(u.port) || (u.protocol === 'https:' ? 443 : 80)
        const tls = u.protocol === 'https:'
        const path = u.pathname + u.search
        const send = async (cookie: string | null): Promise<IdorReply & { bytes: number }> => {
          const payload = buildRawRequest(base.method, path, withCookie(base.headers, cookie), base.body, `${host}:${port}`)
          const buf = await sendRawOverSocket(host, port, payload, { tls })
          const parsed = parseRawResponse(buf)
          return { label: 'baseline', status: parsed.status, body: parsed.body, bytes: parsed.body.length }
        }
        const baseline = { ...(await send(owner)), label: 'baseline' as const }
        const noauth = { ...(await send(null)), label: 'noauth' as const }
        const cross = { ...(await send(alt)), label: 'cross' as const }
        const { bola, note } = idorDecision(baseline, noauth, cross)
        return ok(
          JSON.stringify(
            {
              bola,
              note,
              results: [baseline, noauth, cross].map((r) => ({ label: r.label, status: r.status, bytes: r.bytes }))
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
