import { z } from 'zod'
import { createHash } from 'node:crypto'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { repeaterSendRaw, buildRequestSpec, type RepeaterRequestSpec } from '../../repeater'
import { ok, err, errorMessage } from '../utils'

const DEFAULT_MARKER = '§§'

function sha1Short(s: string): string {
  return createHash('sha1').update(s).digest('hex').slice(0, 12)
}

function substitute(spec: RepeaterRequestSpec, marker: string, payload: string): RepeaterRequestSpec {
  return {
    url: spec.url.replaceAll(marker, payload),
    method: spec.method,
    headers: Object.fromEntries(
      Object.entries(spec.headers).map(([k, v]) => [k, v.replaceAll(marker, payload)])
    ),
    body: spec.body !== undefined ? spec.body.replaceAll(marker, payload) : undefined
  }
}

export function registerIntruderTools(mcp: McpServer) {
  mcp.registerTool(
    'intruder_run',
    {
      description:
        `Burp-Intruder-style fuzzer. Insert a marker (default "${DEFAULT_MARKER}") in the URL/headers/body of a base request, supply a list of payloads, and this tool fires one request per payload via the active browser context. Returns a table of {payload, status, length, durationMs, bodyHash} ideal for boolean/timing-oracle blind SQLi or auth-bypass enumeration.`,
      inputSchema: {
        requestId: z.string().describe('Base requestId (from list_requests)'),
        payloads: z.array(z.string()).describe('Payloads to substitute at the marker'),
        marker: z.string().optional().describe(`Marker token (default "${DEFAULT_MARKER}")`),
        concurrency: z
          .number()
          .int()
          .positive()
          .max(20)
          .optional()
          .describe('Parallel workers (default 4)'),
        diffBaseline: z
          .boolean()
          .optional()
          .describe('Also send the original request once for diffing (default true)')
      }
    },
    async ({ requestId, payloads, marker = DEFAULT_MARKER, concurrency = 4, diffBaseline = true }) => {
      try {
        const baseSpec = buildRequestSpec(requestId, undefined)
        const containsMarker =
          baseSpec.url.includes(marker) ||
          (baseSpec.body ?? '').includes(marker) ||
          Object.values(baseSpec.headers).some((v) => v.includes(marker))
        if (!containsMarker) {
          return err(
            `marker "${marker}" not present in URL/body/headers of base request — add the marker first via repeater or supply a different marker.`
          )
        }

        const results: Array<{
          payload: string
          status: number
          length: number
          durationMs: number
          bodyHash: string
          error?: string
        }> = []
        let baseline: typeof results[number] | undefined

        if (diffBaseline) {
          // baseline: substitute marker with empty string
          const baseRes = await repeaterSendRaw(substitute(baseSpec, marker, ''))
          baseline = {
            payload: '<baseline:empty>',
            status: baseRes.status,
            length: baseRes.bodyByteLength,
            durationMs: baseRes.timeMs,
            bodyHash: sha1Short(baseRes.body),
            error: baseRes.error
          }
        }

        const queue = [...payloads]
        async function worker() {
          while (queue.length > 0) {
            const payload = queue.shift()
            if (payload === undefined) break
            try {
              const res = await repeaterSendRaw(substitute(baseSpec, marker, payload))
              results.push({
                payload,
                status: res.status,
                length: res.bodyByteLength,
                durationMs: res.timeMs,
                bodyHash: sha1Short(res.body),
                error: res.error
              })
            } catch (e) {
              results.push({
                payload,
                status: 0,
                length: 0,
                durationMs: 0,
                bodyHash: '',
                error: errorMessage(e)
              })
            }
          }
        }
        await Promise.all(Array.from({ length: concurrency }, () => worker()))

        // re-sort to original payload order
        const idx = new Map(payloads.map((p, i) => [p, i]))
        results.sort((a, b) => (idx.get(a.payload) ?? 0) - (idx.get(b.payload) ?? 0))

        return ok(
          JSON.stringify(
            {
              marker,
              baseRequestId: requestId,
              baseline,
              count: results.length,
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
