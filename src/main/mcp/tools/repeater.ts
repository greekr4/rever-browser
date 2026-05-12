import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { repeaterSend } from '../../repeater'
import { ok, err, errorMessage } from '../utils'

export function registerRepeaterTools(mcp: McpServer) {
  mcp.registerTool(
    'repeater_send',
    {
      description:
        'Replay a captured request with optional modifications. Uses the active webview context (cookies, TLS, HTTP/2) so it behaves like the real browser. Returns response status, headers, and body (first 64KB). Note: forbidden fetch headers (Cookie, Host, User-Agent, Origin, Referer, sec-*) are stripped — Cookie is auto-attached from the browser jar via credentials=include.',
      inputSchema: {
        requestId: z.string().describe('requestId returned by list_requests'),
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
