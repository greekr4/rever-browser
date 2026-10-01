import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { listRequests, getRequest } from '../../traffic-store'
import { toHeaders } from '../har-build'
import { ok, err, errorMessage } from '../utils'

// per-entry 본문 최대 256KB, 전체 JSON 응답 최대 50MB
const BODY_TRUNCATE_BYTES = 256 * 1024
const HAR_RESPONSE_SIZE_LIMIT = 50 * 1024 * 1024
// MCP Streamable HTTP sends the response as a single SSE event, and widely
// used clients (e.g. httpx2) enforce a 1MB per-event cap — beyond that the
// stream ends with no response and the client only sees "SSE stream ended
// without a response". Keep output under that bound and let callers fetch
// the older remainder via the log._nextBefore/_nextBeforeId cursor, so a
// misjudged `limit` yields a truncated result instead of a dead stream.
const SSE_SAFE_RESPONSE_BYTES = 900 * 1024

export function registerHarTools(mcp: McpServer) {
  mcp.registerTool(
    'har_export',
    {
      description:
        'Export captured traffic as HAR 1.2 JSON. Suitable for loading into Burp Suite, Caido, or any HAR-compatible analyzer. ' +
        'If the output would exceed the client SSE event-size limit it is automatically truncated to the newest entries — ' +
        'the response then contains log._nextBefore/_nextBeforeId: re-call with before/beforeId set to those values ' +
        'to continue fetching the older remainder.',
      inputSchema: {
        host: z.string().optional().describe('Substring host filter'),
        limit: z.number().int().positive().max(2000).optional().describe('Max entries (default 500)'),
        includeBodies: z
          .boolean()
          .optional()
          .describe('Include response bodies (default true, may be large)'),
        before: z
          .number()
          .optional()
          .describe(
            'Boundary cursor: only include entries strictly older than the entry with this startedAt (epoch-ms). ' +
              'Lets a client page through large traffic sets in safe-sized chunks (avoids MCP SSE client ' +
              'event-size limits) — when the server auto-truncates a response, log._nextBefore/_nextBeforeId ' +
              'carry exactly the values to pass here. Pair with beforeId to disambiguate entries sharing the ' +
              'same millisecond.'
          ),
        beforeId: z
          .string()
          .optional()
          .describe(
            'requestId of the boundary entry, paired with `before`. Excludes the boundary entry itself and ' +
              'newer siblings sharing its startedAt so every page is guaranteed to make progress.'
          )
      }
    },
    async ({ host, limit, includeBodies = true, before, beforeId }) => {
      try {
        const entries = listRequests({ host, limit: limit ?? 500, before, beforeId })
        const harEntries: Record<string, unknown>[] = []
        // Boundary candidates for the next page — pushed only for entries that
        // actually make it into harEntries, so the two arrays stay aligned even
        // when an entry is skipped below.
        const cursors: { startedAt: number; requestId: string }[] = []
        let skippedEntries = 0
        entries.forEach((r) => {
          const full = getRequest(r.requestId) ?? r
          const startedDateTime = new Date(full.startedAt).toISOString()
          const timeMs = full.completedAt ? full.completedAt - full.startedAt : -1

          const reqHeaders = toHeaders(full.requestHeaders)
          const respHeaders = toHeaders(full.responseHeaders)

          // A single entry whose URL won't parse (chrome://, devtools://,
          // empty) used to kill the whole export with "Invalid URL" — skip
          // it per-entry and surface the count on the log instead.
          let queryString: { name: string; value: string }[]
          try {
            const reqUrl = new URL(full.url)
            queryString = Array.from(reqUrl.searchParams.entries()).map(([name, value]) => ({
              name,
              value
            }))
          } catch {
            skippedEntries += 1
            return
          }

          harEntries.push({
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
          })
          cursors.push({ startedAt: full.startedAt, requestId: r.requestId })
        })

        const har: { log: Record<string, unknown> } = {
          log: {
            version: '1.2',
            creator: { name: 'rever-browser', version: '0.1.0' },
            entries: harEntries,
            ...(skippedEntries > 0 ? { _skippedEntries: skippedEntries } : {})
          }
        }

        // All size checks measure BYTES — String.length counts UTF-16 code
        // units, which undercounts multibyte bodies (one Korean char is 3
        // UTF-8 bytes) and would let an over-limit response reach the stream.
        let serialized = JSON.stringify(har, null, 2)
        let byteLength = Buffer.byteLength(serialized)

        // Over the SSE-safe bound, keep the newest entries that fit the byte
        // budget in a single pass — halving drops more than needed and
        // re-stringifies the whole document each round.
        if (byteLength > SSE_SAFE_RESPONSE_BYTES && harEntries.length > 1) {
          const entryBytes = harEntries.map((e) => Buffer.byteLength(JSON.stringify(e)))
          // Envelope ≈ document size minus the sum of its parts.
          const envelope = Math.max(0, byteLength - entryBytes.reduce((a, b) => a + b, 0))
          let kept = 0
          let used = envelope
          while (kept < harEntries.length && used + entryBytes[kept] <= SSE_SAFE_RESPONSE_BYTES) {
            used += entryBytes[kept]
            kept++
          }
          // Per-entry stringify runs slightly under the embedded size
          // (deeper indentation, separators, the truncation fields), so
          // verify the assembled document and trim the oldest if it ran low.
          while (kept >= 1) {
            har.log.entries = harEntries.slice(0, kept)
            if (kept < harEntries.length) {
              har.log._truncated = true
              har.log._droppedEntries = harEntries.length - kept
              har.log._nextBefore = cursors[kept - 1].startedAt
              har.log._nextBeforeId = cursors[kept - 1].requestId
              har.log._continuationHint =
                'Response was cut to fit the MCP SSE event-size limit. Re-call har_export ' +
                'with before=_nextBefore and beforeId=_nextBeforeId to fetch the older remainder.'
            }
            serialized = JSON.stringify(har, null, 2)
            byteLength = Buffer.byteLength(serialized)
            if (byteLength <= SSE_SAFE_RESPONSE_BYTES) break
            kept--
          }
        }
        if (byteLength > SSE_SAFE_RESPONSE_BYTES) {
          return err(
            `HAR output exceeds the SSE-safe size (${byteLength} bytes) even with a single entry. ` +
              'Fetch it directly with get_request instead.'
          )
        }
        if (byteLength > HAR_RESPONSE_SIZE_LIMIT) {
          return err(
            `HAR output too large (${byteLength} bytes). Reduce limit or use host filter.`
          )
        }
        return ok(serialized)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
