import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { ok } from '../utils'
import { listWasm, decompileRequest } from '../wasm-analysis'

export function registerWasmTools(mcp: McpServer): void {
  mcp.registerTool(
    'list_wasm',
    {
      description:
        'List captured WebAssembly modules (application/wasm bodies or .wasm URLs) from the traffic store, largest first. Use this to find the requestId to pass to wasm_decompile.',
      inputSchema: { host: z.string().optional().describe('Only modules whose host contains this') }
    },
    async ({ host }) => {
      const rows = listWasm(host).map((r) => ({
        requestId: r.requestId,
        url: r.url,
        bytes: r.responseBody
          ? r.responseBodyBase64
            ? Math.floor(r.responseBody.length * 0.75)
            : r.responseBody.length
          : 0,
        captured: !!r.responseBody,
        truncated: !!r.responseBodyTruncated
      }))
      if (rows.length === 0)
        return ok('No WASM modules captured yet. Load the page that fetches the .wasm, then retry.')
      return ok(JSON.stringify(rows, null, 2))
    }
  )

  mcp.registerTool(
    'wasm_decompile',
    {
      description:
        'Disassemble a captured WebAssembly module to text. Default output is WAT (wasm2wat — the guaranteed, always-available baseline). Pass full:true for a higher-level, C-like decompilation (wasm-decompile) that is easier to read for locating signing/crypto logic; if that path is unavailable it degrades to WAT. Get the requestId from list_wasm.',
      inputSchema: {
        requestId: z.string().describe('requestId of a captured WASM module (from list_wasm)'),
        full: z
          .boolean()
          .optional()
          .describe('true → wasm-decompile (C-like); default false → wasm2wat (WAT)')
      }
    },
    async ({ requestId, full }) => decompileRequest(requestId, !!full)
  )
}
