import { listRequests, type StoredRequest } from '../traffic-store'
import { API_TYPES, endpointKey } from './endpoints'

// How many correlated requests to list inline before summarising the rest.
const MAX_TRIGGERED = 20
// Lookback used to learn what the page was already sending on its own.
const BACKGROUND_WINDOW_MS = 10_000
// An endpoint seen this often in the lookback is background traffic (polling,
// logging beacons). Once is not enough: pressing the same button twice must
// still show its request the second time.
const BACKGROUND_MIN_HITS = 2

// [start, end] of recent agent actions. Requests an action fired are its own,
// not background traffic, so they must not teach the noise filter.
type Window = [number, number]
const MAX_WINDOWS = 50
const actionWindows: Window[] = []

/** Drop requests that started inside a past agent action window. */
export function outsideActions(reqs: StoredRequest[], windows: readonly Window[]): StoredRequest[] {
  return reqs.filter((r) => !windows.some(([a, b]) => r.startedAt >= a && r.startedAt <= b))
}

export interface Triggered {
  shown: StoredRequest[]
  hidden: number
}

/**
 * Pure filter for the requests an action set off: XHR/Fetch only, the acting
 * tab only (other tabs share the store), minus endpoints that were already
 * recurring before the action. Hidden ones are counted, never dropped silently.
 */
export function filterTriggered(
  candidates: StoredRequest[],
  background: StoredRequest[],
  tabId: number | undefined
): Triggered {
  const sameTab = (r: StoredRequest) => tabId == null || r.tabId == null || r.tabId === tabId
  const hits = new Map<string, number>()
  for (const r of background) {
    if (!API_TYPES.has(r.resourceType) || !sameTab(r)) continue
    const key = endpointKey(r)
    if (key) hits.set(key, (hits.get(key) ?? 0) + 1)
  }
  const shown: StoredRequest[] = []
  let hidden = 0
  for (const r of candidates) {
    if (!API_TYPES.has(r.resourceType) || !sameTab(r)) continue
    const key = endpointKey(r)
    if (key && (hits.get(key) ?? 0) >= BACKGROUND_MIN_HITS) hidden++
    else shown.push(r)
  }
  shown.sort((a, b) => a.startedAt - b.startedAt)
  return { shown, hidden }
}

/**
 * Render the requests an action set off as a compact block appended to a
 * browser tool result — closes the "I clicked, but which request fired?" gap
 * without a separate list_requests round-trip. Always emits `since` so the
 * agent has a real clock value for a follow-up list_requests.
 */
export function formatTriggeredRequests({ shown, hidden }: Triggered, since: number, max = MAX_TRIGGERED): string {
  const hiddenNote = hidden
    ? `\n(${hidden} recurring background request(s) hidden — list_requests(${JSON.stringify({ since })}) shows everything)`
    : ''
  if (shown.length === 0) {
    return `\n\n--- no XHR/Fetch requests triggered by this action (since=${since}) ---${hiddenNote}`
  }
  const lines = shown.slice(0, max).map((r) => `#${r.requestId} ${r.method} ${r.status ?? '—'} ${r.url}`)
  const more = shown.length > max ? `\n… +${shown.length - max} more` : ''
  return `\n\n--- requests triggered by this action (${shown.length}, since=${since}) ---\n${lines.join('\n')}${more}${hiddenNote}`
}

/** Store lookup + filter for the action that started at `since` on tab `tabId`. */
export function collectTriggeredRequests(since: number, tabId: number | undefined): Triggered {
  const recent = listRequests({ since: since - BACKGROUND_WINDOW_MS, limit: 500 })
  const result = filterTriggered(
    recent.filter((r) => r.startedAt >= since),
    outsideActions(
      recent.filter((r) => r.startedAt < since),
      actionWindows
    ),
    tabId
  )
  actionWindows.push([since, Date.now()])
  if (actionWindows.length > MAX_WINDOWS) actionWindows.shift()
  return result
}
