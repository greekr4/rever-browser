import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { repeaterSend } from '../../repeater'
import { ok, err, errorMessage } from '../utils'

export function registerRepeaterTools(mcp: McpServer) {
  mcp.registerTool(
    'repeater_send',
    {
      description:
        'Send an HTTP request from the active tab (cookies, TLS, HTTP/2 — behaves like the real browser): replay a captured request (requestId) with optional modifications, OR omit requestId and give modifications.url (+ method/setHeaders/body) to send to a URL that was never captured. Use this instead of curl. The fetch runs in the active tab, so navigate to the target origin first to avoid CORS. Returns status, headers, and body (first 64KB). Forbidden fetch headers (Cookie, Host, User-Agent, Origin, Referer, sec-*) are stripped — Cookie is auto-attached from the browser jar.',
      inputSchema: {
        requestId: z
          .string()
          .optional()
          .describe('requestId from list_requests. Omit to send a fresh request to modifications.url'),
        modifications: z
          .object({
            url: z.string().optional(),
            method: z.string().optional(),
            setHeaders: z
              .record(z.string(), z.string())
              .optional()
              .describe('Headers to add or overwrite (case-insensitive replace)'),
            removeHeaders: z
              .array(z.string())
              .optional()
              .describe('Header names to remove (case-insensitive)'),
            body: z
              .string()
              .nullable()
              .optional()
              .describe('null clears body; omit to keep original')
          })
          .optional()
      }
    },
    async ({ requestId, modifications }) => {
      try {
        const res = await repeaterSend(requestId, modifications)
        return ok(JSON.stringify(res, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
