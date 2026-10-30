import { templatePath } from '../../shared/endpoint-template'
import type { StoredRequest } from '../traffic-store'
import { registrableDomain } from './scope'

// Same set the renderer's API map aggregates — documents, assets and pings are not endpoints.
export const API_TYPES = new Set(['XHR', 'Fetch'])

/** `METHOD origin/templated-path`, or null for an unparsable URL. */
export function endpointKey(r: Pick<StoredRequest, 'method' | 'url'>): string | null {
  try {
    const u = new URL(r.url)
    return `${r.method} ${u.origin}${templatePath(u.pathname)}`
  } catch {
    return null
  }
}

export interface EndpointSummary {
  method: string
  origin: string
  path: string
  count: number
  statuses: number[]
  lastRequestId: string
  lastSeen: number
}

/** Group XHR/Fetch requests by method + origin + templated path, most-called first. */
export function summarizeEndpoints(reqs: StoredRequest[]): EndpointSummary[] {
  const byKey = new Map<string, EndpointSummary>()
  for (const r of reqs) {
    if (!API_TYPES.has(r.resourceType)) continue
    let u: URL
    try {
      u = new URL(r.url)
    } catch {
      continue
    }
    const path = templatePath(u.pathname)
    const key = `${r.method} ${u.origin}${path}`
    const ep = byKey.get(key) ?? {
      method: r.method,
      origin: u.origin,
      path,
      count: 0,
      statuses: [],
      lastRequestId: r.requestId,
      lastSeen: -Infinity
    }
    ep.count++
    if (r.status != null && !ep.statuses.includes(r.status)) ep.statuses.push(r.status)
    if (r.startedAt >= ep.lastSeen) {
      ep.lastSeen = r.startedAt
      ep.lastRequestId = r.requestId
    }
    byKey.set(key, ep)
  }
  return [...byKey.values()].sort((a, b) => b.count - a.count || b.lastSeen - a.lastSeen)
}

/** eTLD+1 of an http(s) URL; null otherwise. localhost:3000 and :8080 share "localhost". */
function siteOf(url: string): string | null {
  try {
    const u = new URL(url)
    return u.protocol === 'http:' || u.protocol === 'https:' ? registrableDomain(u.hostname) : null
  } catch {
    return null
  }
}

/**
 * The store outlives navigation, so a tab that visited another site still holds
 * its traffic. Keep the current page's site; the rest is returned for counting.
 */
export function splitBySite(
  reqs: StoredRequest[],
  pageUrl: string | undefined
): { site: string | null; same: StoredRequest[]; other: StoredRequest[] } {
  const site = pageUrl ? siteOf(pageUrl) : null
  if (!site) return { site, same: reqs, other: [] }
  const same: StoredRequest[] = []
  const other: StoredRequest[] = []
  for (const r of reqs) (siteOf(r.url) === site ? same : other).push(r)
  return { site, same, other }
}

export function formatEndpoints(
  eps: EndpointSummary[],
  limit: number,
  scope?: { site: string; otherSites: number }
): string {
  const hidden = scope?.otherSites
    ? `\n(${scope.otherSites} endpoint(s) on other sites hidden — pass all=true to include them)`
    : ''
  if (eps.length === 0) return `No XHR/Fetch endpoints captured yet${scope ? ` for site ${scope.site}` : ''}.${hidden}`
  const lines = eps.slice(0, limit).map((e) => {
    const statuses = e.statuses.length ? ` [${[...e.statuses].sort((a, b) => a - b).join(',')}]` : ''
    return `${e.method} ${e.origin}${e.path} ×${e.count}${statuses} last=#${e.lastRequestId}`
  })
  const more = eps.length > limit ? `\n… +${eps.length - limit} more` : ''
  const where = scope ? ` on site ${scope.site}` : ''
  return `${eps.length} endpoint(s)${where}, most-called first. get_request the last=# id for details.\n${lines.join('\n')}${more}${hidden}`
}
