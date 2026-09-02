import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getRequest, listRequests } from '../../traffic-store'
import { diffRequests, computeApiBase, type Change } from '../request-diff'
import { ok, err } from '../utils'

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

      const changes: Change[] = diffRequests(reqA, reqB)

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

      const candidates = computeApiBase(all.map((r) => r.url))
      return ok(JSON.stringify({ totalAnalyzed: all.length, candidates }, null, 2))
    }
  )
}
