// Capture-not-send fence — P1 of docs/feature-plan-red-team-ui-crawl.md.
//
// While a Red Team UI crawl holds the fence, a browser-originated request that
// would change server state is answered with a synthetic 204 and recorded,
// instead of reaching the server. The endpoint's shape is already captured by
// Network.* (requestWillBeSent fires before Fetch.requestPaused), which is the
// whole point; the server side stays untouched. Idempotent reads pass through.
//
// Blocked = any non-idempotent method, or an auth-invalidation path (logout /
// revoke) at any method so a stray click can't log the scan out mid-run. Scope
// is enforced by the caller (chrome-cdp) — the decision here is method/path
// only. The hold is ref-counted so parallel per-tab crawls can each take it; it
// lifts only when the last releases.

export type FenceReason = 'mutation' | 'auth-invalidation'

export interface FenceBlock {
  method: string
  url: string
  ts: number
  reason: FenceReason
}

let holders = 0
const blocks: FenceBlock[] = []
const BLOCKS_CAP = 1000

const MUTATION_METHODS = new Set(['POST', 'PUT', 'PATCH', 'DELETE'])
// Session-ending paths, blocked at any method.
const AUTH_INVALIDATION = /(?:^|[/._-])(?:logout|log-?off|sign-?out|signoff|revoke|deauth|unauth)(?:[/._?&=-]|$)/i

export function acquireFence(): void {
  if (holders === 0) blocks.length = 0 // fresh ledger per crawl session
  holders++
}

export function releaseFence(): void {
  if (holders > 0) holders--
}

export function isFenceActive(): boolean {
  return holders > 0
}

/** Whether this request mutates server state (so the fence must hold it). */
export function fenceReason(method: string, url: string): FenceReason | null {
  let path = url
  try {
    path = new URL(url).pathname
  } catch {}
  if (AUTH_INVALIDATION.test(path)) return 'auth-invalidation'
  if (MUTATION_METHODS.has(method.toUpperCase())) return 'mutation'
  return null
}

export function recordFenceBlock(method: string, url: string, reason: FenceReason): void {
  blocks.push({ method, url, ts: Date.now(), reason })
  if (blocks.length > BLOCKS_CAP) blocks.splice(0, blocks.length - BLOCKS_CAP)
}

export function getFenceBlocks(): FenceBlock[] {
  return [...blocks]
}
