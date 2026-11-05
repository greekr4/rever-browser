import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { listRequests, getWsFrames, getRequest } from '../../traffic-store'
import { ok, err } from '../utils'
import { installWsCapture, listWsCaptures, sendWsFrame } from '../../ws-capture'

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
      // requestId 존재 여부 먼저 확인 — 없으면 명확한 에러 반환
      if (!getRequest(requestId)) {
        return err(`unknown requestId: ${requestId} — use list_websockets to find valid IDs`)
      }
      const frames = getWsFrames(requestId, since)
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

  mcp.registerTool(
    'ws_capture_enable',
    {
      description:
        'Install a WebSocket constructor wrapper on the active page so future "new WebSocket(...)" calls are tracked. Required before ws_send can target a socket by index. Page must be reloaded for the wrapper to catch pre-existing sockets.'
    },
    async () => {
      const r = await installWsCapture()
      return r.ok ? ok('ws-capture installed; reload page to wrap pre-existing sockets') : err(r.error)
    }
  )

  mcp.registerTool(
    'ws_capture_list',
    {
      description: 'List WebSocket sockets captured by ws_capture_enable.'
    },
    async () => {
      const r = await listWsCaptures()
      if ('error' in r) return err(r.error)
      return ok(JSON.stringify(r, null, 2))
    }
  )

  mcp.registerTool(
    'ws_send',
    {
      description:
        'Send a frame on a captured WebSocket. Use ws_capture_enable first (and reload), then ws_capture_list to find the captureId. Payload is sent as a UTF-8 string. For binary, base64-encode the bytes and set asBinary=true.',
      inputSchema: {
        captureId: z.number().int().nonnegative().describe('id from ws_capture_list'),
        payload: z.string().describe('Message payload (text or base64 when asBinary=true)'),
        asBinary: z.boolean().optional().describe('Treat payload as base64-encoded bytes')
      }
    },
    async ({ captureId, payload, asBinary }) => {
      const r = await sendWsFrame(captureId, payload, asBinary ?? false)
      if (!r.ok) return err(r.error)
      return ok(r.message)
    }
  )
}
