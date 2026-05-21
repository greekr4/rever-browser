import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getActiveTarget } from '../../chrome-cdp'
import { listRequests, getWsFrames } from '../../traffic-store'
import { ok, err, errorMessage } from '../utils'

const MAX_PAYLOAD_BYTES = 1024

const WS_CAPTURE_SCRIPT = `(() => {
  if (window.__revWSCapture) return;
  const list = [];
  window.__revWSCapture = list;
  const OrigWS = window.WebSocket;
  function Wrapped(url, protocols) {
    const ws = protocols == null ? new OrigWS(url) : new OrigWS(url, protocols);
    const entry = { id: list.length, url: String(url), ws, openedAt: Date.now() };
    list.push(entry);
    return ws;
  }
  Wrapped.prototype = OrigWS.prototype;
  Wrapped.CONNECTING = OrigWS.CONNECTING;
  Wrapped.OPEN = OrigWS.OPEN;
  Wrapped.CLOSING = OrigWS.CLOSING;
  Wrapped.CLOSED = OrigWS.CLOSED;
  window.WebSocket = Wrapped;
})();`

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

  mcp.registerTool(
    'ws_capture_enable',
    {
      description:
        'Install a WebSocket constructor wrapper on the active page so future "new WebSocket(...)" calls are tracked. Required before ws_send can target a socket by index. Page must be reloaded for the wrapper to catch pre-existing sockets.'
    },
    async () => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        await target.dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', {
          source: WS_CAPTURE_SCRIPT
        })
        // Also try to install on current document (will be a no-op if already
        // there or if document scripts already created sockets via OrigWS).
        await target.dbg.sendCommand('Runtime.evaluate', {
          expression: WS_CAPTURE_SCRIPT
        })
        return ok('ws-capture installed; reload page to wrap pre-existing sockets')
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'ws_capture_list',
    {
      description: 'List WebSocket sockets captured by ws_capture_enable.'
    },
    async () => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        const r = (await target.dbg.sendCommand('Runtime.evaluate', {
          expression: `JSON.stringify((window.__revWSCapture || []).map(e => ({ id: e.id, url: e.url, openedAt: e.openedAt, readyState: e.ws.readyState })))`,
          returnByValue: true
        })) as { result: { value?: string } }
        return ok(r.result.value ?? '[]')
      } catch (e) {
        return err(errorMessage(e))
      }
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
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        const expr = asBinary
          ? `(() => {
              const e = (window.__revWSCapture || [])[${captureId}];
              if (!e) return 'no-such-capture';
              if (e.ws.readyState !== 1) return 'not-open:' + e.ws.readyState;
              const raw = atob(${JSON.stringify(payload)});
              const buf = new Uint8Array(raw.length);
              for (let i = 0; i < raw.length; i++) buf[i] = raw.charCodeAt(i);
              e.ws.send(buf);
              return 'ok';
            })()`
          : `(() => {
              const e = (window.__revWSCapture || [])[${captureId}];
              if (!e) return 'no-such-capture';
              if (e.ws.readyState !== 1) return 'not-open:' + e.ws.readyState;
              e.ws.send(${JSON.stringify(payload)});
              return 'ok';
            })()`
        const r = (await target.dbg.sendCommand('Runtime.evaluate', {
          expression: expr,
          returnByValue: true
        })) as { result: { value?: string } }
        const v = r.result.value
        if (v === 'ok') return ok(`sent to ws#${captureId}`)
        return err(`ws_send failed: ${v}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
