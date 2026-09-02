import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { decodeToken } from '../token-decode'
import { ok, err, errorMessage } from '../utils'

export function registerDecodeTools(mcp: McpServer) {
  mcp.registerTool(
    'decode_token',
    {
      description:
        'Detect and decode a token value: JWT (3-segment base64url), base64, URL-encoded JSON, or hex. Returns type, decoded content, and expiry for JWTs.',
      inputSchema: {
        value: z.string().describe('Token or encoded string to decode')
      }
    },
    async ({ value }) => {
      try {
        return ok(JSON.stringify(decodeToken(value), null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
