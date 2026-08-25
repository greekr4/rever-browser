import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { ok, err, errorMessage } from '../utils'
import {
  listWasm,
  decompileRequest,
  getWasmBuffer,
  extractWasmStrings,
  wasmObjdump,
  xrefExports,
  type WasmFormat
} from '../wasm-analysis'

export function registerWasmTools(mcp: McpServer): void {
  mcp.registerTool(
    'list_wasm',
    {
      description:
        'List captured WebAssembly modules (application/wasm bodies or .wasm URLs) from the traffic store, largest first. Use this to find the requestId to pass to the other wasm_* tools.',
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
        'Disassemble a captured WebAssembly module to text. `format:"wat"` (default) is the guaranteed in-process baseline; `"decompile"` is a higher-level C-like view (wasm-decompile); `"c"` is full C source (wasm2c), the most readable for complex crypto/signing routines. Non-WAT formats degrade to WAT if unavailable. Get the requestId from list_wasm.',
      inputSchema: {
        requestId: z.string().describe('requestId of a captured WASM module (from list_wasm)'),
        format: z
          .enum(['wat', 'decompile', 'c'])
          .optional()
          .describe('wat (default) | decompile (C-like) | c (full C source via wasm2c)'),
        full: z
          .boolean()
          .optional()
          .describe('deprecated alias: full:true == format:"decompile"')
      }
    },
    async ({ requestId, format, full }) => {
      const fmt: WasmFormat = format ?? (full ? 'decompile' : 'wat')
      return decompileRequest(requestId, fmt)
    }
  )

  mcp.registerTool(
    'wasm_info',
    {
      description:
        'Summarize a captured WASM module (wasm-objdump -x): its types, imports, exports, and — when a name section is present — the real function/symbol names. Use this first to see the export surface and pick which function to decompile.',
      inputSchema: {
        requestId: z.string().describe('requestId of a captured WASM module (from list_wasm)')
      }
    },
    async ({ requestId }) => {
      const bytes = getWasmBuffer(requestId)
      if (typeof bytes === 'string') return err(bytes)
      try {
        return ok(await wasmObjdump(bytes))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'grep_wasm',
    {
      description:
        'Extract printable strings/constants from a captured WASM body (a `strings`-style scan of the data section): algorithm ids, embedded keys, URLs, symbol names. Optionally filter by a regex. Fast way to spot signing logic without reading the whole disassembly.',
      inputSchema: {
        requestId: z.string().describe('requestId of a captured WASM module (from list_wasm)'),
        pattern: z.string().optional().describe('Only strings matching this JS regex source'),
        min: z.number().optional().describe('Minimum run length (default 4)')
      }
    },
    async ({ requestId, pattern, min }) => {
      const bytes = getWasmBuffer(requestId)
      if (typeof bytes === 'string') return err(bytes)
      try {
        const strings = extractWasmStrings(bytes, { pattern, min })
        if (strings.length === 0) return ok('No matching strings found.')
        return ok(JSON.stringify(strings, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'wasm_xref',
    {
      description:
        "Cross-reference a WASM module's exported functions against captured JS: for each export, grep the script bodies for its name and show where it is called. Bridges the WASM ↔ JS gap — e.g. finding the JS call site that invokes a WASM signing export.",
      inputSchema: {
        requestId: z.string().describe('requestId of a captured WASM module (from list_wasm)')
      }
    },
    async ({ requestId }) => {
      try {
        const res = await xrefExports(requestId)
        if (typeof res === 'string') return err(res)
        return ok(JSON.stringify(res, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
