import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { listRequests } from '../../traffic-store'
import { scanRequest } from '../secret-scan'
import { ok } from '../utils'

export function registerSecretTools(mcp: McpServer) {
  mcp.registerTool(
    'scan_secrets',
    {
      description:
        'Sweep captured traffic for embedded credentials and secrets — JWTs, API keys (Google/AWS/GitHub/Slack/Stripe), private keys, Bearer tokens, and api_key/secret/password assignments — across response bodies, request bodies, and sensitive request headers (Authorization/Cookie/x-api-key…). Values are masked (first/last 4 chars). A quick way to find a hardcoded key or endpoint that shortcuts the whole reversing effort. Filter by host / since to scope the sweep.',
      inputSchema: {
        host: z.string().optional().describe('Substring host filter (e.g. "example.com")'),
        since: z.number().optional().describe('Only scan requests started after this epoch ms')
      }
    },
    async ({ host, since }) => {
      const rows = listRequests({ host, since, limit: 200 })
      const results = rows
        .map((e) => ({ requestId: e.requestId, url: e.url, hits: scanRequest(e) }))
        .filter((r) => r.hits.length > 0)
      const total = results.reduce((n, r) => n + r.hits.length, 0)
      return ok(
        JSON.stringify(
          {
            scanned: rows.length,
            requestsWithSecrets: results.length,
            totalHits: total,
            results
          },
          null,
          2
        )
      )
    }
  )
}
