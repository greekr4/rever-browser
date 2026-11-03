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

import { scopeKey } from './scope'

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

// Loopback = your own machine, so mutating it during a red-team scan is safe and
// expected (local dev app, a localhost CTF/benchmark). The fence steps aside for
// loopback BY DEFAULT, so the red team CONFIRMS mutation-only vulns (SSTI,
// SQLi/cmd-injection via POST, auth bypass) out of the box instead of stopping at
// "candidate". Every non-loopback host stays fenced unless explicitly marked.
export function isLoopbackHost(url: string): boolean {
  let hostname: string
  try {
    hostname = new URL(url).hostname.toLowerCase()
  } catch {
    return false
  }
  return (
    hostname === 'localhost' ||
    hostname.endsWith('.localhost') ||
    hostname === '::1' ||
    hostname === '[::1]' ||
    // Full 127.0.0.0/8 IPv4 only — anchored so "127.0.0.1.evil.com" (a public
    // DNS name that merely starts with 127.) is NOT treated as loopback.
    /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(hostname)
  )
}

// Extra lab targets — non-loopback hosts the user owns and has explicitly marked
// safe to mutate (e.g. an authorized staging domain), via the lab_target tool.
// Stored as scopeKey()s so loopback-style entries are port-specific; a bare host
// opens all its ports. Opt-in and off by default.
let labHosts: string[] = []

export function setLabHosts(hosts: string[]): string[] {
  const out: string[] = []
  for (const h of hosts) {
    const key = scopeKey(h)
    if (key && !out.includes(key)) out.push(key)
  }
  labHosts = out
  return [...labHosts]
}

export function getLabHosts(): string[] {
  return [...labHosts]
}

export function clearLabHosts(): void {
  labHosts = []
}

/** Whether `url`'s host was marked a disposable lab target (fence steps aside). */
export function isLabHost(url: string): boolean {
  if (labHosts.length === 0) return false
  const key = scopeKey(url)
  if (!key) return false
  const bare = key.split(':')[0]
  return labHosts.includes(key) || (bare !== key && labHosts.includes(bare))
}

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
  // Disposable lab target (loopback by default, or explicitly marked): allow
  // mutations through so the red team can confirm the vuln.
  if (isLoopbackHost(url) || isLabHost(url)) return null
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
