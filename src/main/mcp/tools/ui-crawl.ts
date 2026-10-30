import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import {
  getActiveTarget,
  getTargetByIndex,
  getInterceptRules,
  runInTarget,
  waitForSettle,
  type AttachedTarget
} from '../../chrome-cdp'
import { acquireFence, releaseFence, fenceReason } from '../fence'
import { getScope, scopeBlockForUrl } from '../scope'
import { clickRef, takeSnapshot, navigateHistory } from '../snapshot'
import { pressKey } from '../human-input'
import { collectTriggeredRequests } from '../triggered-requests'
import { ok, err, errorMessage } from '../utils'

// P2 of docs/feature-plan-red-team-ui-crawl.md — the exhaustive click crawler.
// A deterministic loop (the code presses, the AI never decides what to skip):
// snapshot → click each actionable element once → attribute the requests it
// fired → recover (navigate back / close a modal) → repeat. The P1 fence makes
// it safe: mutating requests are captured and answered with a 204, never sent.

const ACTION_WINDOW_MS = 500
const DEFAULT_MAX_ELEMENTS = 40
const HARD_MAX_ELEMENTS = 150
const MAX_MS = 60_000

type ElemStatus = 'clicked' | 'blocked-destructive' | 'failed' | 'navigated' | 'noop'

interface ElemResult {
  label: string
  status: ElemStatus
  endpoints: string[] // "METHOD origin+path" signatures
}

interface Actionable {
  ref: string
  label: string
  key: string
}

// Pull actionable nodes (the ones snapshot.ts tagged with [ref=rN]) out of the
// YAML-like outline. The label (role + accessible name) is a stable identity
// across re-snapshots — refs are reassigned each snapshot, labels are not — so
// it doubles as the visited key. Same-labelled elements collapse to one, which
// matches "several buttons firing the same request are one finding".
export function parseActionable(tree: string): Actionable[] {
  const out: Actionable[] = []
  const seen = new Set<string>()
  for (const raw of tree.split('\n')) {
    const m = /\[ref=(r\d+)\]/.exec(raw)
    if (!m) continue
    const ref = m[1]
    const label = raw
      .replace(/\s*\[ref=r\d+\].*$/, '')
      .replace(/^[\s\-•|`]+/, '')
      .trim()
    const key = label || ref
    if (seen.has(key)) continue
    seen.add(key)
    out.push({ ref, label, key })
  }
  return out
}

// method + origin + path, query stripped — the endpoint identity for dedup and
// for feeding the probe phase (scan.ts parseEndpoints reads "METHOD URL").
export function endpointSig(method: string, url: string): string {
  let clean = url
  try {
    const u = new URL(url)
    clean = u.origin + u.pathname
  } catch {
    clean = url.replace(/[?#].*$/, '')
  }
  return `${method.toUpperCase()} ${clean}`
}

// Whether a triggered request was (or would be) held by the fence — the same
// decision chrome-cdp applies: in-scope + mutating.
function wasBlocked(method: string, url: string): boolean {
  return getScope().enabled && scopeBlockForUrl(url) === null && fenceReason(method, url) !== null
}

async function pageUrl(target: AttachedTarget): Promise<string> {
  try {
    const r = (await target.dbg.sendCommand('Runtime.evaluate', {
      expression: 'location.href',
      returnByValue: true
    })) as { result?: { value?: string } }
    return r.result?.value ?? ''
  } catch {
    return ''
  }
}

async function crawl(
  target: AttachedTarget,
  budget: number
): Promise<{ startUrl: string; elements: ElemResult[]; endpoints: string[] }> {
  const tabId = target.wc.id
  const visited = new Set<string>()
  const elements: ElemResult[] = []
  const endpoints = new Set<string>()
  const deadline = Date.now() + MAX_MS
  let startUrl = ''

  let clicks = 0
  while (clicks < budget && Date.now() < deadline) {
    let tree = ''
    let url = ''
    try {
      const snap = await takeSnapshot()
      tree = snap.tree
      url = snap.url
    } catch {
      break
    }
    if (!startUrl) startUrl = url

    const next = parseActionable(tree).find((a) => !visited.has(a.key))
    if (!next) break
    visited.add(next.key)
    clicks++

    const since = Date.now() - ACTION_WINDOW_MS
    try {
      await clickRef(next.ref)
    } catch {
      elements.push({ label: next.label, status: 'failed', endpoints: [] })
      continue
    }
    await waitForSettle()

    const triggered = collectTriggeredRequests(since, tabId).shown
    const eps: string[] = []
    let blockedAny = false
    for (const r of triggered) {
      const sig = endpointSig(r.method, r.url)
      if (!eps.includes(sig)) eps.push(sig)
      endpoints.add(sig)
      if (wasBlocked(r.method, r.url)) blockedAny = true
    }

    // Recover: a navigation leaves the crawl on the wrong page — go back; a
    // same-page click may have opened a modal — Escape closes the common ones.
    let navigated = false
    const after = await pageUrl(target)
    if (after && url && after !== url) {
      navigated = true
      try {
        await navigateHistory('back')
        await waitForSettle()
      } catch {}
    } else {
      try {
        await pressKey('Escape')
      } catch {}
    }

    const status: ElemStatus = eps.length
      ? blockedAny
        ? 'blocked-destructive'
        : 'clicked'
      : navigated
        ? 'navigated'
        : 'noop'
    elements.push({ label: next.label, status, endpoints: eps })
  }

  return { startUrl, elements, endpoints: [...endpoints] }
}

function formatCrawl(tab: number | undefined, r: { startUrl: string; elements: ElemResult[]; endpoints: string[] }): string {
  const by = (s: ElemStatus): number => r.elements.filter((e) => e.status === s).length
  const summary =
    `elements: ${r.elements.length} — ` +
    `clicked ${by('clicked')}, blocked-destructive ${by('blocked-destructive')}, ` +
    `failed ${by('failed')}, navigated ${by('navigated')}, noop ${by('noop')}`
  const map = r.elements
    .filter((e) => e.endpoints.length)
    .map((e) => {
      const tag = e.status === 'blocked-destructive' ? '  [blocked]' : ''
      return `  "${e.label}" → ${e.endpoints.join(', ')}${tag}`
    })
    .join('\n')
  return (
    `--- ui_crawl${tab != null ? ` (tab ${tab})` : ''} ---\n` +
    `page: ${r.startUrl}\n` +
    `${summary}\n` +
    (map ? `element → API map:\n${map}\n` : '') +
    `endpoints discovered:\n${JSON.stringify(r.endpoints)}`
  )
}

export function registerUiCrawlTools(mcp: McpServer): void {
  mcp.registerTool(
    'ui_crawl',
    {
      description:
        'Red Team exhaustive-click crawler: deterministically clicks every actionable element on the page once, attributes the requests each fired (button→API map), and returns the discovered endpoints for the probe phase. Mutating requests are captured and answered with a synthetic 204 by the capture-not-send fence — the server state is never changed. Pass `tab` (browser_tabs index) to crawl a background tab.',
      inputSchema: {
        tab: z.number().int().optional().describe('Tab index from browser_tabs (default: the selected tab)'),
        maxElements: z
          .number()
          .int()
          .optional()
          .describe(`Max elements to click (default ${DEFAULT_MAX_ELEMENTS}, hard cap ${HARD_MAX_ELEMENTS})`)
      }
    },
    async ({ tab, maxElements }) => {
      const target = tab != null ? getTargetByIndex(tab) : getActiveTarget()
      if (!target) {
        return err(tab != null ? `no tab at index ${tab} — call browser_tabs first` : 'no active browser target')
      }
      const budget = Math.min(Math.max(maxElements ?? DEFAULT_MAX_ELEMENTS, 1), HARD_MAX_ELEMENTS)
      return runInTarget(target, async () => {
        acquireFence()
        const hadRules = getInterceptRules().length > 0
        try {
          await enableFenceFetch(target)
          const result = await crawl(target, budget)
          return ok(formatCrawl(tab, result))
        } catch (e) {
          return err(errorMessage(e))
        } finally {
          await restoreFetch(target, hadRules)
          releaseFence()
        }
      })
    }
  )
}

// Fetch.enable on this tab with a catch-all so the fence sees every request;
// existing user intercept rules are preserved alongside it.
async function enableFenceFetch(target: AttachedTarget): Promise<void> {
  const patterns = getInterceptRules().map((r) => ({ urlPattern: r.urlPattern, requestStage: r.stage }))
  patterns.push({ urlPattern: '*', requestStage: 'Request' as const })
  await target.dbg.sendCommand('Fetch.enable', { patterns }).catch(() => {})
}

// Put this tab's Fetch state back the way the crawl found it.
async function restoreFetch(target: AttachedTarget, hadRules: boolean): Promise<void> {
  if (!hadRules) {
    await target.dbg.sendCommand('Fetch.disable').catch(() => {})
    return
  }
  const patterns = getInterceptRules().map((r) => ({ urlPattern: r.urlPattern, requestStage: r.stage }))
  await target.dbg.sendCommand('Fetch.enable', { patterns }).catch(() => {})
}
