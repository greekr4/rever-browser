import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getRequest, listRequests } from '../../traffic-store'
import { ok, err } from '../utils'

function parseQuery(url: string): Record<string, string> {
  try {
    const u = new URL(url)
    const obj: Record<string, string> = {}
    u.searchParams.forEach((v, k) => { obj[k] = v })
    return obj
  } catch {
    return {}
  }
}

function diffObjects(
  label: string,
  a: Record<string, unknown>,
  b: Record<string, unknown>
): Array<{ type: 'added' | 'removed' | 'changed'; key: string; from?: unknown; to?: unknown }> {
  const changes: Array<{ type: 'added' | 'removed' | 'changed'; key: string; from?: unknown; to?: unknown }> = []
  const allKeys = new Set([...Object.keys(a), ...Object.keys(b)])
  for (const k of allKeys) {
    const inA = k in a
    const inB = k in b
    if (!inA) changes.push({ type: 'added', key: `${label}.${k}`, to: b[k] })
    else if (!inB) changes.push({ type: 'removed', key: `${label}.${k}`, from: a[k] })
    else if (JSON.stringify(a[k]) !== JSON.stringify(b[k]))
      changes.push({ type: 'changed', key: `${label}.${k}`, from: a[k], to: b[k] })
  }
  return changes
}

function tryParseJson(s: string | undefined): Record<string, unknown> | null {
  if (!s) return null
  try {
    const parsed = JSON.parse(s)
    if (typeof parsed === 'object' && parsed !== null) return parsed as Record<string, unknown>
    return null
  } catch {
    return null
  }
}

export function registerDiffTools(mcp: McpServer) {
  mcp.registerTool(
    'request_diff',
    {
      description:
        'Compare two captured requests (URL query params, headers, post body). Shows added/removed/changed keys.',
      inputSchema: {
        a: z.string().describe('First requestId'),
        b: z.string().describe('Second requestId')
      }
    },
    async ({ a, b }) => {
      const reqA = getRequest(a)
      const reqB = getRequest(b)
      if (!reqA) return err(`unknown requestId: ${a}`)
      if (!reqB) return err(`unknown requestId: ${b}`)

      const changes: ReturnType<typeof diffObjects> = []

      // URL query params
      const qA = parseQuery(reqA.url)
      const qB = parseQuery(reqB.url)
      changes.push(...diffObjects('query', qA, qB))

      // Headers
      const hA = reqA.requestHeaders ?? {}
      const hB = reqB.requestHeaders ?? {}
      // Normalize header keys to lowercase
      const hANorm = Object.fromEntries(Object.entries(hA).map(([k, v]) => [k.toLowerCase(), v]))
      const hBNorm = Object.fromEntries(Object.entries(hB).map(([k, v]) => [k.toLowerCase(), v]))
      changes.push(...diffObjects('headers', hANorm, hBNorm))

      // Post body (JSON key-level diff if possible)
      const bodyA = tryParseJson(reqA.requestPostData)
      const bodyB = tryParseJson(reqB.requestPostData)
      if (bodyA && bodyB) {
        changes.push(...diffObjects('body', bodyA, bodyB))
      } else if (reqA.requestPostData !== reqB.requestPostData) {
        changes.push({
          type: 'changed',
          key: 'body',
          from: reqA.requestPostData,
          to: reqB.requestPostData
        })
      }

      const summary = {
        a: { requestId: a, url: reqA.url, method: reqA.method },
        b: { requestId: b, url: reqB.url, method: reqB.method },
        changes: {
          added: changes.filter((c) => c.type === 'added'),
          removed: changes.filter((c) => c.type === 'removed'),
          changed: changes.filter((c) => c.type === 'changed')
        }
      }
      return ok(JSON.stringify(summary, null, 2))
    }
  )

  mcp.registerTool(
    'find_api_base',
    {
      description:
        'Analyze captured XHR/Fetch requests and find the most common host+path-prefix combinations — useful to identify the API base URL.',
      inputSchema: {
        host: z.string().optional().describe('Restrict to requests from this host substring'),
        limit: z.number().int().positive().max(500).optional().describe('Max requests to analyze (default 200)')
      }
    },
    async ({ host, limit }) => {
      const requests = listRequests({ host, methodOrType: 'Fetch', limit: limit ?? 200 })
      const xhrRequests = listRequests({ host, methodOrType: 'XHR', limit: limit ?? 200 })
      const all = [...requests, ...xhrRequests]

      // Count host + first 2 path segments
      const counts = new Map<string, number>()
      for (const r of all) {
        try {
          const u = new URL(r.url)
          const segments = u.pathname.split('/').filter(Boolean)
          const prefix = segments.slice(0, 2).join('/')
          const key = `${u.protocol}//${u.host}/${prefix}`
          counts.set(key, (counts.get(key) ?? 0) + 1)
        } catch {}
      }

      const sorted = [...counts.entries()]
        .sort((a, b) => b[1] - a[1])
        .slice(0, 5)
        .map(([base, count]) => ({ base, count }))

      return ok(JSON.stringify({ totalAnalyzed: all.length, candidates: sorted }, null, 2))
    }
  )
}
