import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getRequest, listRequests, type StoredRequest } from '../../traffic-store'
import { ok, okFullOrSummary, err } from '../utils'
import { formatEndpoints, splitBySite, summarizeEndpoints } from '../endpoints'
import { getActiveTarget, listTargets } from '../../chrome-cdp'

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

// The agent addresses tabs by the index it sees in browser_tabs; the store keys
// traffic by webContents id. Map one to the other, or null for a bad index.
function tabIdForIndex(index: number | undefined): number | null | undefined {
  if (index == null) return undefined
  const t = listTargets()[index]
  return t ? t.id : null
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
        tab: z.number().int().optional().describe('Only this tab, by its index in browser_tabs (default: all tabs)'),
        limit: z.number().int().positive().max(200).optional().describe('Max items (default 50)'),
        source: z
          .enum(['live', 'import', 'all'])
          .optional()
          .describe('live = captured in this app (default), import = loaded with import_har, all = both')
      }
    },
    async ({ tab, ...args }) => {
      const tabId = tabIdForIndex(tab)
      if (tabId === null) return err(`no tab at index ${tab} — call browser_tabs first`)
      const rows = listRequests({ ...args, tabId }).map(toSummary)
      return ok(JSON.stringify(rows, null, 2))
    }
  )

  mcp.registerTool(
    'list_endpoints',
    {
      description:
        'Map of the API endpoints seen so far: captured XHR/Fetch requests grouped by method + origin + path (numeric/hex/uuid segments become :id), one line each with call count, status codes and the latest requestId. Defaults to the current page\'s site (eTLD+1; other sites visited earlier are counted, not listed). Far smaller than list_requests — call this first to find the API, then get_request the last=# id.',
      inputSchema: {
        host: z.string().optional().describe('Substring host filter (e.g. "api.example.com"); replaces the current-site default'),
        all: z.boolean().optional().describe('Include every site in the store, not just the current page\'s'),
        tab: z.number().int().optional().describe('Only endpoints captured in this tab, by its browser_tabs index'),
        limit: z.number().int().positive().max(200).optional().describe('Max endpoints listed (default 50)')
      }
    },
    async ({ host, all, tab, limit }) => {
      const tabId = tabIdForIndex(tab)
      if (tabId === null) return err(`no tab at index ${tab} — call browser_tabs first`)
      // The live ring buffer holds 500 entries, so this scans all of it.
      const rows = listRequests({ host, tabId, limit: 500 })
      if (host || all) return ok(formatEndpoints(summarizeEndpoints(rows), limit ?? 50))
      // When a tab is named, scope the site by THAT tab's URL, not the active one.
      const pageUrl = tab != null ? listTargets()[tab]?.url : getActiveTarget()?.wc.getURL()
      const { site, same, other } = splitBySite(rows, pageUrl)
      const scope = site ? { site, otherSites: summarizeEndpoints(other).length } : undefined
      return ok(formatEndpoints(summarizeEndpoints(same), limit ?? 50, scope))
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
