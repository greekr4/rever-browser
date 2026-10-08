import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getRequest, listRequests, type StoredRequest } from '../../traffic-store'
import { ok, okFullOrSummary, err } from '../utils'

// Top-level keys of a JSON response body, for the summary view. Returns null
// when the body isn't present/decodable JSON.
function topLevelJsonKeys(entry: StoredRequest): string[] | null {
  if (entry.responseBodyBase64 || !entry.responseBody) return null
  try {
    const parsed = JSON.parse(entry.responseBody)
    if (parsed && typeof parsed === 'object') {
      return Array.isArray(parsed) ? [`(array, length ${parsed.length})`] : Object.keys(parsed)
    }
  } catch {
    return null
  }
  return null
}

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
        limit: z.number().int().positive().max(200).optional().describe('Max items (default 50)'),
        source: z
          .enum(['live', 'import', 'all'])
          .optional()
          .describe('live = captured in this app (default), import = loaded with import_har, all = both')
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
        'Return a captured request/response incl. headers and body. Small entries come back in full; large ones come back as a compact SUMMARY (method, url, status, content-type, body size, top-level JSON keys) — then pass full=true to page the complete entry via tool_result_more. full=false always returns the summary. If the body is base64-encoded, responseBodyBase64=true.',
      inputSchema: {
        requestId: z.string().describe('requestId returned by list_requests'),
        full: z
          .boolean()
          .optional()
          .describe('true = full entry (paged when large), false = summary only, omit = full when small')
      }
    },
    async ({ requestId, full }) => {
      const entry = getRequest(requestId)
      if (!entry) return err(`unknown requestId: ${requestId}`)
      return okFullOrSummary(JSON.stringify(entry, null, 2), () => ({
        requestId: entry.requestId,
        method: entry.method,
        url: entry.url,
        host: entry.host,
        status: entry.status,
        mimeType: entry.mimeType,
        encodedDataLength: entry.encodedDataLength,
        responseBodyBase64: entry.responseBodyBase64 ?? false,
        bodyChars: entry.responseBody?.length ?? 0,
        responseBodyTopKeys: topLevelJsonKeys(entry),
        hint: 'entry too large for one response — call get_request with full=true to page headers and body'
      }), full)
    }
  )

  mcp.registerTool(
    'get_request_initiator',
    {
      description:
        'Show what caused a captured request to fire: the initiator type (script/parser/preload/other) and, for script-initiated XHR/Fetch, the JavaScript call stack (top frame = the code that directly issued it). Use this to jump from a request straight to the script:line that built it, before setting a breakpoint. Line/column are 1-based for readability.',
      inputSchema: {
        requestId: z.string().describe('requestId returned by list_requests')
      }
    },
    async ({ requestId }) => {
      const entry = getRequest(requestId)
      if (!entry) return err(`unknown requestId: ${requestId}`)
      const stack = (entry.initiatorStack ?? []).map((f) => ({
        functionName: f.functionName || '(anonymous)',
        url: f.url,
        line: f.lineNumber + 1,
        column: f.columnNumber + 1,
        location: `${f.url}:${f.lineNumber + 1}:${f.columnNumber + 1}`
      }))
      const top = stack[0]
      return ok(
        JSON.stringify(
          {
            requestId: entry.requestId,
            method: entry.method,
            url: entry.url,
            initiatorType: entry.initiatorType ?? 'unknown',
            initiatorUrl: entry.initiatorUrl,
            firedFrom: top?.location,
            stack,
            note:
              stack.length === 0
                ? 'No JS call stack — this request was not script-initiated (e.g. parser/document, preload, or a redirect). initiatorType tells you which.'
                : undefined
          },
          null,
          2
        )
      )
    }
  )
}
