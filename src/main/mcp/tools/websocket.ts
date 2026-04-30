import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { listRequests, getWsFrames } from '../../traffic-store'
import { ok, err } from '../utils'

const MAX_PAYLOAD_BYTES = 1024

export function registerWebSocketTools(mcp: McpServer) {
  mcp.registerTool(
    'list_websockets',
    {
      description: 'List captured WebSocket connections.',
      inputSchema: {
        host: z.string().optional().describe('Substring host filter'),
        limit: z.number().int().positive().max(200).optional().describe('Max items (default 50)')
      }
    },
    async ({ host, limit }) => {
      const all = listRequests({ host, limit: limit ?? 50 })
      const ws = all.filter((r) => r.resourceType === 'WebSocket')
      return ok(
        JSON.stringify(
          ws.map((r) => ({
            requestId: r.requestId,
            url: r.url,
            host: r.host,
            startedAt: r.startedAt,
            completedAt: r.completedAt
          })),
          null,
          2
        )
      )
    }
  )

  mcp.registerTool(
    'get_ws_frames',
    {
      description: 'Return WebSocket frames for a given connection. Large payloads are truncated at 1KB.',
      inputSchema: {
        requestId: z.string().describe('requestId of the WebSocket connection'),
        since: z.number().optional().describe('Only include frames after this epoch ms'),
        limit: z.number().int().positive().max(500).optional().describe('Max frames (default 100)')
      }
    },
    async ({ requestId, since, limit }) => {
      const frames = getWsFrames(requestId, since)
      if (frames.length === 0 && !listRequests({ limit: 1 }).some(() => true)) {
        return err(`no WebSocket frames found for requestId: ${requestId}`)
      }
      const sliced = frames.slice(-(limit ?? 100))
      const formatted = sliced.map((f) => {
        const payload = f.payloadData
        const truncated = payload.length > MAX_PAYLOAD_BYTES
        return {
          direction: f.direction,
          opcode: f.opcode,
          timestamp: f.timestamp,
          mask: f.mask,
          payload: truncated ? payload.slice(0, MAX_PAYLOAD_BYTES) : payload,
          ...(truncated ? { truncated: true, totalBytes: payload.length } : {})
        }
      })
      return ok(JSON.stringify(formatted, null, 2))
    }
  )
}
