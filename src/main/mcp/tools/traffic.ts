import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getRequest, listRequests, type StoredRequest } from '../../traffic-store'
import { ok, err } from '../utils'

function toSummary(e: StoredRequest) {
  return {
    requestId: e.requestId,
    method: e.method,
    url: e.url,
    host: e.host,
    resourceType: e.resourceType,
    status: e.status,
    mimeType: e.mimeType,
    encodedDataLength: e.encodedDataLength,
    startedAt: e.startedAt
  }
}

export function registerTrafficTools(mcp: McpServer) {
  mcp.registerTool(
    'list_requests',
    {
      description:
        'List recent network requests captured by the browser, newest first. Filter by host / method / type / since. Response headers and body are NOT included — call get_request for details.',
      inputSchema: {
        host: z.string().optional().describe('Substring host filter (e.g. "danawa.com")'),
        methodOrType: z
          .string()
          .optional()
          .describe(
            'Substring match against HTTP method (GET/POST...) or ResourceType (XHR/Fetch/Document...)'
          ),
        since: z.number().optional().describe('Only include requests started after this epoch ms'),
        limit: z.number().int().positive().max(200).optional().describe('Max items (default 50)')
      }
    },
    async (args) => {
      const rows = listRequests(args).map(toSummary)
      return ok(JSON.stringify(rows, null, 2))
    }
  )

  mcp.registerTool(
    'get_request',
    {
      description:
        'Return the full request and response for a given requestId, including headers and body. If the body is base64-encoded, responseBodyBase64=true.',
      inputSchema: {
        requestId: z.string().describe('requestId returned by list_requests')
      }
    },
    async ({ requestId }) => {
      const entry = getRequest(requestId)
      if (!entry) return err(`unknown requestId: ${requestId}`)
      return ok(JSON.stringify(entry, null, 2))
    }
  )
}
