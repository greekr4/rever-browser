import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { buildRequestSpec, repeaterSendRaw } from '../../repeater'
import { ok, err, errorMessage } from '../utils'

const CRLF_PAYLOADS = [
  '\r\nX-Injected: 1',
  '%0d%0aX-Injected:%201',
  '%0d%0aSet-Cookie:%20injected=1',
  '\r\n\r\n<script>alert(1)</script>',
  '%E5%98%8A%E5%98%8DX-Injected: 1', // unicode CR/LF bypass
  '%0aSet-Cookie:%20pwn=1'
]

export function registerCrlfTools(mcp: McpServer) {
  mcp.registerTool(
    'crlf_test',
    {
      description:
        'Inject CRLF payloads into a request URL/body/header (marker `§`) and report whether injected headers appear in the response (header injection / log forgery / open redirect chains).',
      inputSchema: {
        requestId: z.string().describe('Base requestId with § marker'),
        payloads: z.array(z.string()).optional().describe('Override default CRLF payloads')
      }
    },
    async ({ requestId, payloads }) => {
      try {
        const base = buildRequestSpec(requestId, undefined)
        const containsMarker =
          base.url.includes('§') ||
          (base.body ?? '').includes('§') ||
          Object.values(base.headers).some((v) => v.includes('§'))
        if (!containsMarker) return err('marker § not found in request')

        const list = payloads ?? CRLF_PAYLOADS
        const results: Array<{
          payload: string
          status: number
          injectedHeaderPresent: boolean
          responseHeaders: Record<string, string>
          bodyHasScript: boolean
        }> = []

        for (const p of list) {
          const sub = {
            url: base.url.replaceAll('§', p),
            method: base.method,
            headers: Object.fromEntries(
              Object.entries(base.headers).map(([k, v]) => [k, v.replaceAll('§', p)])
            ),
            body: base.body?.replaceAll('§', p)
          }
          const r = await repeaterSendRaw(sub)
          const hasInjected = Object.keys(r.headers).some((h) => h.toLowerCase() === 'x-injected')
          results.push({
            payload: p,
            status: r.status,
            injectedHeaderPresent: hasInjected,
            responseHeaders: r.headers,
            bodyHasScript: r.body.includes('<script>alert(1)</script>')
          })
        }

        return ok(
          JSON.stringify(
            {
              vulnerable: results.some((r) => r.injectedHeaderPresent || r.bodyHasScript),
              results
            },
            null,
            2
          )
        )
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
