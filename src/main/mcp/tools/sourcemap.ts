import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'
import { TraceMap, originalPositionFor, sourceContentFor } from '@jridgewell/trace-mapping'

import { getRequest } from '../../traffic-store'
import { ok, err, errorMessage } from '../utils'

// Module-level source map cache
const traceMapCache = new Map<string, TraceMap>()

function offsetToLineCol(body: string, offset: number): { line: number; column: number } {
  let line = 0
  let col = 0
  for (let i = 0; i < Math.min(offset, body.length); i++) {
    if (body[i] === '\n') {
      line++
      col = 0
    } else {
      col++
    }
  }
  return { line, column: col }
}

async function loadSourceMap(scriptRequestId: string): Promise<TraceMap | { error: string }> {
  const cached = traceMapCache.get(scriptRequestId)
  if (cached) return cached

  const entry = getRequest(scriptRequestId)
  if (!entry) return { error: `unknown requestId: ${scriptRequestId}` }
  if (!entry.responseBody) return { error: `no response body for ${scriptRequestId}` }
  if (entry.responseBodyBase64) return { error: `body is base64 (binary) for ${scriptRequestId}` }

  // Find sourceMappingURL comment
  const match = entry.responseBody.match(/\/\/[#@]\s*sourceMappingURL=([^\s]+)/)
  if (!match) return { error: `no sourceMappingURL found in ${scriptRequestId}` }
  const mappingUrl = match[1].trim()

  let mapJson: string
  if (mappingUrl.startsWith('data:')) {
    // data:application/json;base64,<...>
    const b64match = mappingUrl.match(/base64,(.+)$/)
    if (b64match) {
      mapJson = Buffer.from(b64match[1], 'base64').toString('utf8')
    } else {
      // data:application/json,...
      const commaIdx = mappingUrl.indexOf(',')
      mapJson = commaIdx !== -1 ? decodeURIComponent(mappingUrl.slice(commaIdx + 1)) : ''
    }
  } else {
    // External URL — resolve relative to script URL
    let fetchUrl = mappingUrl
    if (!mappingUrl.startsWith('http')) {
      try {
        fetchUrl = new URL(mappingUrl, entry.url).href
      } catch {
        return { error: `cannot resolve source map URL: ${mappingUrl}` }
      }
    }
    try {
      const resp = await fetch(fetchUrl)
      if (!resp.ok) return { error: `failed to fetch source map ${fetchUrl}: ${resp.status}` }
      mapJson = await resp.text()
    } catch (e) {
      return { error: `fetch source map failed: ${errorMessage(e)}` }
    }
  }

  try {
    const parsed = JSON.parse(mapJson)
    const tm = new TraceMap(parsed)
    traceMapCache.set(scriptRequestId, tm)
    return tm
  } catch (e) {
    return { error: `failed to parse source map: ${errorMessage(e)}` }
  }
}

export function registerSourceMapTools(mcp: McpServer) {
  mcp.registerTool(
    'resolve_source',
    {
      description:
        'Resolve a byte offset or line/column in a captured script to its original source file and position using the embedded source map.',
      inputSchema: {
        requestId: z.string().describe('requestId of the captured script'),
        byteOffset: z.number().int().nonnegative().optional().describe('Byte offset in the bundle (takes precedence over line/column)'),
        line: z.number().int().nonnegative().optional().describe('0-indexed line number'),
        column: z.number().int().nonnegative().optional().describe('0-indexed column number')
      }
    },
    async ({ requestId, byteOffset, line, column }) => {
      try {
        const tm = await loadSourceMap(requestId)
        if ('error' in tm) return err(tm.error)

        let resolvedLine = line ?? 0
        let resolvedCol = column ?? 0

        if (byteOffset != null) {
          const entry = getRequest(requestId)
          if (entry?.responseBody) {
            const pos = offsetToLineCol(entry.responseBody, byteOffset)
            resolvedLine = pos.line
            resolvedCol = pos.column
          }
        }

        const orig = originalPositionFor(tm, { line: resolvedLine, column: resolvedCol })

        // Get snippet from sourcesContent if available
        let snippet: string | undefined
        if (orig.source != null) {
          const content = sourceContentFor(tm, orig.source)
          if (content) {
            const lines = content.split('\n')
            const sl = orig.line ?? 0
            const start = Math.max(0, sl - 5)
            const end = Math.min(lines.length, sl + 5)
            snippet = lines.slice(start, end).join('\n')
          }
        }

        return ok(JSON.stringify({ ...orig, snippet }, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'list_sources',
    {
      description: 'List all original source files referenced in a script\'s source map.',
      inputSchema: {
        requestId: z.string().describe('requestId of the captured script')
      }
    },
    async ({ requestId }) => {
      try {
        const tm = await loadSourceMap(requestId)
        if ('error' in tm) return err(tm.error)
        return ok(JSON.stringify({ sources: tm.sources }, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'get_original_source',
    {
      description: 'Return the full original source content for a file listed by list_sources.',
      inputSchema: {
        requestId: z.string().describe('requestId of the captured script'),
        source: z.string().describe('Source file path as returned by list_sources')
      }
    },
    async ({ requestId, source }) => {
      try {
        const tm = await loadSourceMap(requestId)
        if ('error' in tm) return err(tm.error)
        const content = sourceContentFor(tm, source)
        if (content == null) return err(`no sourcesContent for ${source}`)
        return ok(content)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
