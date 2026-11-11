import { session } from 'electron'
import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { buildRequestSpec, restoreMarker, type RepeaterRequestSpec } from '../../repeater'
import { SHARED_PARTITION } from '../../tab-partition'
import { isLoopbackHost, isLabHost } from '../fence'
import { getAltSession, setAltSession } from '../redteam-tenants'
import { ok, err, errorMessage } from '../utils'
import { buildRawRequest, parseRawResponse, sendRawOverSocket } from './raw_http'

/** The owner's live session: cookies the scan's logged-in partition would send
 *  to `url` (httpOnly included). Captured XHRs often omit the Cookie header
 *  (CDP reports httpOnly cookies out-of-band), so this is how a probe gets the
 *  real owner baseline without the agent threading it in. */
async function partitionCookieHeader(url: string): Promise<string | null> {
  try {
    const cookies = await session.fromPartition(SHARED_PARTITION).cookies.get({ url })
    if (!cookies.length) return null
    return cookies.map((c) => `${c.name}=${c.value}`).join('; ')
  } catch {
    return null
  }
}

interface RawReply {
  status: number
  body: string
  bytes: number
}

/** Rebuild the captured request over a raw socket with `cookie` (or none) and
 *  return the parsed response. Raw socket because browser fetch can't override
 *  the Cookie header (forbidden) nor drop httpOnly cookies (credentials:include
 *  re-attaches them), which is exactly what these auth tests must control. */
async function rawReplay(base: RepeaterRequestSpec, u: URL, cookie: string | null): Promise<RawReply> {
  const host = u.hostname
  const port = Number(u.port) || (u.protocol === 'https:' ? 443 : 80)
  const tls = u.protocol === 'https:'
  const payload = buildRawRequest(base.method, u.pathname + u.search, withCookie(base.headers, cookie), base.body, `${host}:${port}`)
  const parsed = parseRawResponse(await sendRawOverSocket(host, port, payload, { tls }))
  return { status: parsed.status, body: parsed.body, bytes: parsed.body.length }
}

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
  label: 'baseline' | 'noauth' | 'cross' | 'bogus'
  status: number
  body: string
}

/** Same cookie names, garbage values — the negative control for broken auth:
 *  an app that validates sessions rejects this; one that ignores identity
 *  (public-by-design) serves the resource anyway. */
export function bogusCookie(owner: string): string {
  return owner
    .split(';')
    .map((p) => {
      const i = p.indexOf('=')
      return i < 0 ? p : `${p.slice(0, i).trim()}=reverbogus0`
    })
    .join('; ')
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

export interface NoauthOutcome {
  brokenAuth: boolean
  note: string
}

/** Broken auth (API2) over a RAW socket, the reliable version of the
 *  replay-with-cookie-blanked check: replay_request can't drop httpOnly cookies
 *  (credentials:include re-attaches them) so it reports false "no-auth 200"s.
 *  Here the no-cookie request is genuinely cookieless; broken auth is confirmed
 *  only when it returns the SAME protected resource the owner sees. */
export function noauthDecision(baseline: IdorReply, noauth: IdorReply, bogus: IdorReply): NoauthOutcome {
  const ok2xx = (s: number): boolean => s >= 200 && s < 300
  if (!ok2xx(baseline.status)) {
    return { brokenAuth: false, note: `baseline ${baseline.status}: the resource isn't readable even with the owner session — can't assess (bad requestId or no owner session in the partition).` }
  }
  if (!(ok2xx(noauth.status) && sameResource(baseline.body, noauth.body))) {
    return { brokenAuth: false, note: `no-cookie request got ${noauth.status}${ok2xx(noauth.status) ? ' with a different/empty body' : ''}: authentication is enforced (not broken auth).` }
  }
  // No-cookie returns the protected resource. Negative control: a GARBAGE cookie
  // must be DENIED. If the garbage cookie is served the SAME resource, the endpoint
  // ignores identity entirely = PUBLIC-by-design (a CTF mail viewer, a taxonomy) —
  // "served with no cookie" alone can't tell that from broken auth, so don't confirm.
  if (ok2xx(bogus.status) && sameResource(baseline.body, bogus.body)) {
    return {
      brokenAuth: false,
      note: `served identically with NO cookie AND a garbage cookie (HTTP ${noauth.status}/${bogus.status}) — the endpoint ignores identity, consistent with PUBLIC-BY-DESIGN, not proven broken auth. Record as candidate (category:endpoint/info) and confirm whether it is meant to be public; a genuine broken-auth leak returns owner-specific data a wrong identity is denied.`
    }
  }
  return { brokenAuth: true, note: `the resource is returned with NO cookie (HTTP ${noauth.status}, same body as the owner) while a GARBAGE cookie is rejected (${bogus.status}) — the server enforces sessions on this path but the no-cookie request bypasses it. Broken auth (API2) confirmed over a raw socket.` }
}

export function registerIdorProbeTools(mcp: McpServer) {
  mcp.registerTool(
    'noauth_probe',
    {
      description:
        'Confirm broken authentication (OWASP API2) reliably — use this instead of replay_request-with-cookie-blanked, which CANNOT drop httpOnly session cookies (browser fetch re-attaches them via credentials:include) and so reports false "no-auth 200"s. Replays the captured request three ways over a raw socket: owner session (baseline, auto-read from the logged-in partition), NO cookie, and a GARBAGE cookie (negative control). Broken auth is confirmed only when the cookieless request returns the SAME protected resource AND the garbage cookie is denied — if the garbage cookie is served the same resource, the endpoint ignores identity (public-by-design, e.g. a shared viewer or taxonomy) and is NOT confirmed. Gated to loopback / lab hosts. A positive is unauthenticated data access — record high/critical; a public-by-design result is a candidate, not a vuln.',
      inputSchema: {
        requestId: z.string().describe('A captured authenticated request to a protected resource'),
        ownerCookie: z.string().optional().describe('Owner Cookie value (defaults to the logged-in partition session)')
      }
    },
    async ({ requestId, ownerCookie }) => {
      try {
        const base = restoreMarker(buildRequestSpec(requestId, undefined))
        let u: URL
        try {
          u = new URL(base.url)
        } catch {
          return err(`captured request has no valid URL: ${base.url}`)
        }
        if (!isLoopbackHost(base.url) && !isLabHost(base.url)) {
          return err(`noauth_probe sends raw sockets, gated to loopback / lab hosts; ${u.hostname} is neither. Mark it a lab target if you own it.`)
        }
        const owner = ownerCookie ?? originalCookie(base.headers) ?? (await partitionCookieHeader(base.url))
        if (!owner) {
          return err('no owner session available (capture had no Cookie and the partition has none for this origin) — pass ownerCookie.')
        }
        const baseline = { ...(await rawReplay(base, u, owner)), label: 'baseline' as const }
        const noauth = { ...(await rawReplay(base, u, null)), label: 'noauth' as const }
        // Negative control: same cookie names, garbage values. A real protected
        // resource denies this; a public-by-design one serves it anyway.
        const bogus = { ...(await rawReplay(base, u, bogusCookie(owner))), label: 'bogus' as const }
        const { brokenAuth, note } = noauthDecision(baseline, noauth, bogus)
        return ok(
          JSON.stringify(
            { brokenAuth, note, results: [baseline, noauth, bogus].map((r) => ({ label: r.label, status: r.status, bytes: r.bytes })) },
            null,
            2
          )
        )
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

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
        const owner = ownerCookie ?? originalCookie(base.headers) ?? (await partitionCookieHeader(base.url))
        if (!owner) {
          return err(
            'no owner session for the baseline (capture had no Cookie and the partition has none for this origin) — pass ownerCookie.'
          )
        }
        const baseline = { ...(await rawReplay(base, u, owner)), label: 'baseline' as const }
        const noauth = { ...(await rawReplay(base, u, null)), label: 'noauth' as const }
        const cross = { ...(await rawReplay(base, u, alt)), label: 'cross' as const }
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
