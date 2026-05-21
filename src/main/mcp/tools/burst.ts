import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { repeaterSend, type RepeaterModifications } from '../../repeater'
import { ok, err, errorMessage } from '../utils'

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0
  const idx = Math.min(sorted.length - 1, Math.floor((sorted.length - 1) * p))
  return sorted[idx]
}

export function registerBurstTools(mcp: McpServer) {
  mcp.registerTool(
    'burst_send',
    {
      description:
        'Race-condition burst: fire N copies of the same request in parallel via the browser context. Useful for TOCTOU bugs (coupon double-spend, stock overrun, session-file races). Returns timing histogram and per-response stats.',
      inputSchema: {
        requestId: z.string().describe('Base requestId'),
        n: z.number().int().positive().max(200).describe('Total requests to fire'),
        parallelism: z
          .number()
          .int()
          .positive()
          .max(50)
          .optional()
          .describe('Concurrent in-flight (default = n, full burst)'),
        modifications: z
          .object({
            url: z.string().optional(),
            method: z.string().optional(),
            setHeaders: z.record(z.string(), z.string()).optional(),
            removeHeaders: z.array(z.string()).optional(),
            body: z.string().nullable().optional()
          })
          .optional()
      }
    },
    async ({ requestId, n, parallelism, modifications }) => {
      try {
        const par = parallelism ?? n
        const startedAt = Date.now()

        const results: Array<{
          status: number
          length: number
          durationMs: number
          startedOffsetMs: number
          error?: string
        }> = []

        let launched = 0
        async function worker() {
          while (launched < n) {
            const myIdx = launched++
            const startOffset = Date.now() - startedAt
            try {
              const res = await repeaterSend(requestId, modifications as RepeaterModifications | undefined)
              results[myIdx] = {
                status: res.status,
                length: res.bodyByteLength,
                durationMs: res.timeMs,
                startedOffsetMs: startOffset,
                error: res.error
              }
            } catch (e) {
              results[myIdx] = {
                status: 0,
                length: 0,
                durationMs: 0,
                startedOffsetMs: startOffset,
                error: errorMessage(e)
              }
            }
          }
        }
        await Promise.all(Array.from({ length: par }, () => worker()))

        const finishedAt = Date.now()
        const durations = results.map((r) => r.durationMs).sort((a, b) => a - b)
        const statusCounts: Record<number, number> = {}
        for (const r of results) statusCounts[r.status] = (statusCounts[r.status] ?? 0) + 1

        return ok(
          JSON.stringify(
            {
              count: results.length,
              wallClockMs: finishedAt - startedAt,
              statusCounts,
              timing: {
                minMs: durations[0] ?? 0,
                p50Ms: percentile(durations, 0.5),
                p95Ms: percentile(durations, 0.95),
                maxMs: durations[durations.length - 1] ?? 0
              },
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
