import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { ok, err, errorMessage } from '../utils'

function decodeBase64Url(s: string): string {
  const padded = s.replace(/-/g, '+').replace(/_/g, '/').padEnd(s.length + ((4 - (s.length % 4)) % 4), '=')
  return Buffer.from(padded, 'base64').toString('utf8')
}

function tryParseJson(s: string): unknown | undefined {
  try {
    return JSON.parse(s)
  } catch {
    return undefined
  }
}

function formatExpiry(payload: unknown): string {
  if (typeof payload !== 'object' || payload === null) return ''
  const p = payload as Record<string, unknown>
  const exp = p['exp']
  if (typeof exp !== 'number') return ''
  const d = new Date(exp * 1000)
  const now = Date.now()
  const expired = d.getTime() < now
  return ` (exp: ${d.toISOString()}${expired ? ' — EXPIRED' : ''})`
}

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
        const v = value.trim()

        // JWT: 3 base64url segments separated by dots
        const jwtParts = v.split('.')
        if (jwtParts.length === 3) {
          try {
            const headerStr = decodeBase64Url(jwtParts[0])
            const payloadStr = decodeBase64Url(jwtParts[1])
            const header = tryParseJson(headerStr)
            const payload = tryParseJson(payloadStr)
            if (header && payload) {
              return ok(
                JSON.stringify(
                  {
                    type: 'jwt',
                    header,
                    payload,
                    note: `signature not verified${formatExpiry(payload)}`,
                    raw: v
                  },
                  null,
                  2
                )
              )
            }
          } catch {}
        }

        // URL-encoded JSON
        if (v.includes('%')) {
          try {
            const decoded = decodeURIComponent(v)
            const parsed = tryParseJson(decoded)
            if (parsed) {
              return ok(JSON.stringify({ type: 'url-encoded-json', decoded: parsed, raw: v }, null, 2))
            }
            return ok(JSON.stringify({ type: 'url-encoded', decoded, raw: v }, null, 2))
          } catch {}
        }

        // base64 (eyJ prefix is common for JSON)
        if (/^[A-Za-z0-9+/=_-]{4,}$/.test(v)) {
          try {
            const decoded = decodeBase64Url(v)
            const parsed = tryParseJson(decoded)
            if (parsed) {
              return ok(JSON.stringify({ type: 'base64-json', decoded: parsed, raw: v }, null, 2))
            }
            // Only return if looks like printable text
            if (/^[\x20-\x7E\t\n\r]+$/.test(decoded)) {
              return ok(JSON.stringify({ type: 'base64', decoded, raw: v }, null, 2))
            }
          } catch {}
        }

        // hex
        if (/^[0-9a-fA-F]{8,}$/.test(v) && v.length % 2 === 0) {
          const decoded = Buffer.from(v, 'hex').toString('utf8')
          return ok(JSON.stringify({ type: 'hex', decoded, raw: v }, null, 2))
        }

        return ok(JSON.stringify({ type: 'unknown', note: 'Could not detect encoding', raw: v }, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
