import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { listRequests, getWsFrames, getRequest } from '../../traffic-store'
import { ok, err } from '../utils'
import { installWsCapture, listPageSockets, sendWsByUrl, sendWsFrame } from '../../ws-capture'

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
        'Legacy: install a WebSocket constructor wrapper (needs a page reload). Only needed for ws_send by captureId — ws_send with url reaches already-open sockets without it.'
    },
    async () => {
      const r = await installWsCapture()
      return r.ok ? ok('ws-capture installed; reload page to wrap pre-existing sockets') : err(r.error)
    }
  )

  mcp.registerTool(
    'ws_capture_list',
    {
      description:
        "List the page's live WebSockets — in the page and in cross-site iframes — found directly in memory. No setup or reload needed. Use a url from here with ws_send."
    },
    async () => {
      const r = await listPageSockets()
      if ('error' in r) return err(r.error)
      return ok(JSON.stringify(r, null, 2))
    }
  )

  mcp.registerTool(
    'ws_send',
    {
      description:
        "Send a frame on the page's own live WebSocket (auth and session preserved). Pass url (from ws_capture_list or list_websockets) — works on sockets the page already opened, including in cross-site iframes, with no setup. Payload is sent as a UTF-8 string; for binary, base64-encode the bytes and set asBinary=true. If the socket has closed, reconnect=true reopens it from the page (same URL, subprotocols, cookies and origin) and sends on the new connection, which shows up in list_websockets under a new requestId. captureId is the legacy path after ws_capture_enable.",
      inputSchema: {
        url: z.string().optional().describe('Socket URL, e.g. wss://host/socket?room=1'),
        captureId: z.number().int().nonnegative().optional().describe('Legacy: id from the ws_capture_enable wrapper'),
        payload: z.string().describe('Message payload (text or base64 when asBinary=true)'),
        asBinary: z.boolean().optional().describe('Treat payload as base64-encoded bytes'),
        reconnect: z.boolean().optional().describe('With url: if the socket has closed, reopen it from the page and send on the new connection')
      }
    },
    async ({ url, captureId, payload, asBinary, reconnect }) => {
      if (url == null && captureId == null) return err('pass url (preferred) or captureId')
      const r = url != null ? await sendWsByUrl(url, payload, asBinary ?? false, reconnect ?? false) : await sendWsFrame(captureId!, payload, asBinary ?? false)
      if (!r.ok) return err(r.error)
      return ok(r.message)
    }
  )
}
