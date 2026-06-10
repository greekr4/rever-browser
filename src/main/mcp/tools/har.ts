import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { listRequests, getRequest } from '../../traffic-store'
import { ok, err, errorMessage } from '../utils'

// per-entry 본문 최대 256KB, 전체 JSON 응답 최대 50MB
const BODY_TRUNCATE_BYTES = 256 * 1024
const HAR_RESPONSE_SIZE_LIMIT = 50 * 1024 * 1024

interface HarHeader {
  name: string
  value: string
}

function toHeaders(h: Record<string, string> | undefined): HarHeader[] {
  if (!h) return []
  return Object.entries(h).map(([name, value]) => ({ name, value }))
}

export function registerHarTools(mcp: McpServer) {
  mcp.registerTool(
    'har_export',
    {
      description:
        'Export captured traffic as HAR 1.2 JSON. Suitable for loading into Burp Suite, Caido, or any HAR-compatible analyzer.',
      inputSchema: {
        host: z.string().optional().describe('Substring host filter'),
        limit: z.number().int().positive().max(2000).optional().describe('Max entries (default 500)'),
        includeBodies: z
          .boolean()
          .optional()
          .describe('Include response bodies (default true, may be large)')
      }
    },
    async ({ host, limit, includeBodies = true }) => {
      try {
        const entries = listRequests({ host, limit: limit ?? 500 })
        const harEntries = entries.map((r) => {
          const full = getRequest(r.requestId) ?? r
          const startedDateTime = new Date(full.startedAt).toISOString()
          const timeMs = full.completedAt ? full.completedAt - full.startedAt : -1

          const reqHeaders = toHeaders(full.requestHeaders)
          const respHeaders = toHeaders(full.responseHeaders)

          const reqUrl = new URL(full.url)
          const queryString = Array.from(reqUrl.searchParams.entries()).map(([name, value]) => ({
            name,
            value
          }))

          return {
            startedDateTime,
            time: timeMs,
            request: {
              method: full.method,
              url: full.url,
              httpVersion: 'HTTP/1.1',
              cookies: [],
              headers: reqHeaders,
              queryString,
              headersSize: -1,
              bodySize: full.requestPostData ? full.requestPostData.length : 0,
              ...(full.requestPostData
                ? {
                    postData: {
                      mimeType:
                        full.requestHeaders?.['content-type'] ??
                        full.requestHeaders?.['Content-Type'] ??
                        'application/octet-stream',
                      text: full.requestPostData
                    }
                  }
                : {})
            },
            response: {
              status: full.status ?? 0,
              statusText: '',
              httpVersion: 'HTTP/1.1',
              cookies: [],
              headers: respHeaders,
              content: {
                size: full.responseBody?.length ?? 0,
                mimeType: full.mimeType ?? '',
                ...(includeBodies && full.responseBody
                  ? (() => {
                      const body = full.responseBody
                      const truncated = body.length > BODY_TRUNCATE_BYTES
                      return {
                        text: truncated ? body.slice(0, BODY_TRUNCATE_BYTES) : body,
                        ...(full.responseBodyBase64 ? { encoding: 'base64' } : {}),
                        ...(truncated ? { comment: `truncated (original ${body.length} bytes)` } : {})
                      }
                    })()
                  : {})
              },
              redirectURL: '',
              headersSize: -1,
              bodySize: full.encodedDataLength ?? -1
            },
            cache: {},
            timings: {
              send: 0,
              wait: timeMs > 0 ? timeMs : -1,
              receive: 0
            }
          }
        })

        const har = {
          log: {
            version: '1.2',
            creator: { name: 'rever-browser', version: '0.1.0' },
            entries: harEntries
          }
        }

        const serialized = JSON.stringify(har, null, 2)
        if (serialized.length > HAR_RESPONSE_SIZE_LIMIT) {
          return err(
            `HAR output too large (${serialized.length} bytes). Reduce limit or use host filter.`
          )
        }
        return ok(serialized)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
