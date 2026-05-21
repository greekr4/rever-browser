import { z } from 'zod'
import { createHash } from 'node:crypto'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { buildRequestSpec, repeaterSendRaw } from '../../repeater'
import { ok, err, errorMessage } from '../utils'

const DEFAULT_XSS_PAYLOADS = [
  '<script>alert(1)</script>',
  '"><svg/onload=alert(1)>',
  "'><img src=x onerror=alert(1)>",
  'javascript:alert(1)',
  '<details/open/ontoggle=alert(1)>',
  '<x onclick=alert(1)>x',
  '<a href="javascript:alert(1)">x</a>'
]

interface Reflection {
  payload: string
  status: number
  length: number
  reflectedRaw: boolean
  reflectedHtmlEscaped: boolean
  reflectedUrlEncoded: boolean
  contextHint: 'attribute' | 'text' | 'script' | 'unknown' | 'none'
  preview: string
}

function detectContext(body: string, payload: string): Reflection['contextHint'] {
  const idx = body.indexOf(payload)
  if (idx === -1) return 'none'
  const before = body.slice(Math.max(0, idx - 80), idx)
  const after = body.slice(idx + payload.length, idx + payload.length + 80)
  if (/<script[^>]*>[^<]*$/i.test(before)) return 'script'
  if (/=\s*"[^"]*$/.test(before) || /=\s*'[^']*$/.test(before)) return 'attribute'
  if (/>[^<]*$/.test(before) || /^[^<]*</.test(after)) return 'text'
  return 'unknown'
}

export function registerPayloadProbeTools(mcp: McpServer) {
  mcp.registerTool(
    'payload_probe',
    {
      description:
        'Fire reflection / stored-XSS probes at a request slot (marker §) and inspect the response body for raw, HTML-escaped, and URL-encoded reflections. Reports the lexical context where the payload landed.',
      inputSchema: {
        requestId: z.string(),
        payloads: z.array(z.string()).optional().describe('Override default XSS payloads')
      }
    },
    async ({ requestId, payloads }) => {
      try {
        const base = buildRequestSpec(requestId, undefined)
        if (
          !base.url.includes('§') &&
          !(base.body ?? '').includes('§') &&
          !Object.values(base.headers).some((v) => v.includes('§'))
        ) {
          return err('marker § not found in request')
        }

        const list = payloads ?? DEFAULT_XSS_PAYLOADS
        const reflections: Reflection[] = []
        for (const p of list) {
          const sub = {
            url: base.url.replaceAll('§', encodeURIComponent(p)),
            method: base.method,
            headers: Object.fromEntries(
              Object.entries(base.headers).map(([k, v]) => [k, v.replaceAll('§', p)])
            ),
            body: base.body?.replaceAll('§', p)
          }
          const r = await repeaterSendRaw(sub)
          const body = r.body
          const reflectedRaw = body.includes(p)
          const htmlEsc = p
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
          reflections.push({
            payload: p,
            status: r.status,
            length: r.bodyByteLength,
            reflectedRaw,
            reflectedHtmlEscaped: body.includes(htmlEsc),
            reflectedUrlEncoded: body.includes(encodeURIComponent(p)),
            contextHint: reflectedRaw ? detectContext(body, p) : 'none',
            preview: reflectedRaw
              ? body.slice(Math.max(0, body.indexOf(p) - 40), body.indexOf(p) + p.length + 40)
              : ''
          })
        }
        return ok(JSON.stringify({ count: reflections.length, reflections }, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
