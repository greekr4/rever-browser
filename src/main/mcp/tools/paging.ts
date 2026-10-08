import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { err, getResultSlice, ok } from '../utils'

export function registerPagingTools(mcp: McpServer) {
  mcp.registerTool(
    'tool_result_more',
    {
      description:
        'Fetch the next slice of a large tool result that was truncated with a "[truncated: … resultId=…]" footer. Pass the resultId and offset from that footer. Returns the slice plus nextOffset (null when done).',
      inputSchema: {
        resultId: z.string().describe('resultId from a truncated result footer'),
        offset: z.number().int().nonnegative().describe('Character offset to resume from'),
        limit: z
          .number()
          .int()
          .positive()
          .max(60_000)
          .optional()
          .describe('Max chars to return (default 12000)')
      }
    },
    async ({ resultId, offset, limit }) => {
      const page = getResultSlice(resultId, offset, limit)
      if (!page) {
        return err(
          `unknown or expired resultId: ${resultId} (results are kept briefly; re-run the original tool)`
        )
      }
      const footer = page.more
        ? `\n\n[more: call tool_result_more resultId="${resultId}", offset=${page.nextOffset} (${page.total - page.end} chars left)]`
        : `\n\n[end of result: ${page.total} chars total]`
      return ok(page.slice + footer)
    }
  )
}
