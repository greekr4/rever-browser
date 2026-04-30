import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getRequest } from '../../traffic-store'
import {
  detectBundler,
  grepBody,
  listScripts,
  patternForCategory,
  runWebcrack,
  type Category
} from '../script-analysis'
import { ok, err, errorMessage } from '../utils'

const CATEGORIES = [
  'api',
  'urls',
  'env',
  'secrets',
  'auth',
  'hooks',
  'fetch',
  'ai',
  'baas',
  'function-calling',
  'rpc'
] as const satisfies readonly Category[]

function getScriptBody(requestId: string): { body: string } | { error: string } {
  const entry = getRequest(requestId)
  if (!entry) return { error: `unknown requestId: ${requestId}` }
  if (!entry.responseBody)
    return { error: `requestId ${requestId} has no captured body (still loading or skipped)` }
  if (entry.responseBodyBase64)
    return { error: `requestId ${requestId} body is base64 (binary) — not a text script` }
  return { body: entry.responseBody }
}

function buildPattern(
  category: Category | undefined,
  pattern: string | undefined
): RegExp | { error: string } {
  if (pattern) {
    try {
      return new RegExp(pattern, 'g')
    } catch (e) {
      return { error: `invalid regex: ${errorMessage(e)}` }
    }
  }
  if (!category) return { error: 'either pattern or category is required' }
  return patternForCategory(category)
}

export function registerScriptTools(mcp: McpServer) {
  mcp.registerTool(
    'list_scripts',
    {
      description:
        'List JavaScript bundles captured from the browser (Script resourceType), largest body first. Use these requestIds with grep_script / extract_context / detect_bundler / deobfuscate_script.',
      inputSchema: {
        host: z.string().optional().describe('Substring host filter (e.g. "vercel.app")'),
        limit: z.number().int().positive().max(200).optional().describe('Max items (default 30)'),
        minSize: z.number().int().nonnegative().optional().describe('Minimum body size in bytes')
      }
    },
    async ({ host, limit, minSize }) => {
      try {
        const rows = listScripts({ host, limit: limit ?? 30, minSize }).map((r) => ({
          requestId: r.requestId,
          url: r.url,
          host: r.host,
          size: r.responseBody?.length ?? 0,
          mimeType: r.mimeType,
          bundler: detectBundler(r.responseBody!).name
        }))
        return ok(JSON.stringify(rows, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'grep_script',
    {
      description:
        'Grep a single captured script for a regex or a preset category. Returns matches with byte offsets and a small surrounding snippet — works on minified files where line-based grep fails.',
      inputSchema: {
        requestId: z.string().describe('From list_scripts'),
        category: z
          .enum(CATEGORIES)
          .optional()
          .describe(
            'Preset pattern: api | urls | env | secrets | auth | hooks | fetch | ai | baas | function-calling | rpc'
          ),
        pattern: z
          .string()
          .optional()
          .describe('Custom JS regex (overrides category). Always treated as global.'),
        maxMatches: z.number().int().positive().max(200).optional().describe('Default 20'),
        contextBefore: z
          .number()
          .int()
          .nonnegative()
          .max(5000)
          .optional()
          .describe('Bytes before match (default 200)'),
        contextAfter: z
          .number()
          .int()
          .nonnegative()
          .max(5000)
          .optional()
          .describe('Bytes after match (default 400)')
      }
    },
    async ({ requestId, category, pattern, maxMatches, contextBefore, contextAfter }) => {
      const got = getScriptBody(requestId)
      if ('error' in got) return err(got.error)
      const re = buildPattern(category as Category | undefined, pattern)
      if (re instanceof RegExp === false && 'error' in re) return err(re.error)
      try {
        const matches = grepBody(got.body, re as RegExp, {
          max: maxMatches ?? 20,
          before: contextBefore ?? 200,
          after: contextAfter ?? 400
        })
        return ok(
          JSON.stringify(
            { requestId, totalShown: matches.length, matches },
            null,
            2
          )
        )
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'grep_scripts',
    {
      description:
        'Grep across ALL captured scripts. Use to locate a symbol/string when you do not yet know which bundle holds it. Returns match per script with byte offsets.',
      inputSchema: {
        category: z.enum(CATEGORIES).optional(),
        pattern: z.string().optional().describe('Custom JS regex (overrides category)'),
        host: z.string().optional().describe('Restrict to scripts from this host substring'),
        maxMatchesPerScript: z.number().int().positive().max(50).optional().describe('Default 5'),
        maxScripts: z.number().int().positive().max(100).optional().describe('Default 30')
      }
    },
    async ({ category, pattern, host, maxMatchesPerScript, maxScripts }) => {
      const re = buildPattern(category as Category | undefined, pattern)
      if (re instanceof RegExp === false && 'error' in re) return err(re.error)
      try {
        const scripts = listScripts({ host, limit: maxScripts ?? 30 })
        const results: Array<{
          requestId: string
          url: string
          matches: ReturnType<typeof grepBody>
        }> = []
        for (const s of scripts) {
          const matches = grepBody(s.responseBody!, re as RegExp, {
            max: maxMatchesPerScript ?? 5,
            before: 120,
            after: 200
          })
          if (matches.length === 0) continue
          results.push({ requestId: s.requestId, url: s.url, matches })
        }
        return ok(
          JSON.stringify(
            {
              scriptsScanned: scripts.length,
              scriptsWithMatches: results.length,
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

  mcp.registerTool(
    'extract_context',
    {
      description:
        'Return a byte-range slice from a captured script. Use after grep_script to read the function body around a hit (the dd-style trick for minified bundles).',
      inputSchema: {
        requestId: z.string(),
        byteOffset: z.number().int().nonnegative(),
        before: z.number().int().nonnegative().max(50_000).optional().describe('Default 500'),
        after: z.number().int().nonnegative().max(50_000).optional().describe('Default 4000')
      }
    },
    async ({ requestId, byteOffset, before, after }) => {
      const got = getScriptBody(requestId)
      if ('error' in got) return err(got.error)
      const b = before ?? 500
      const a = after ?? 4000
      const start = Math.max(0, byteOffset - b)
      const end = Math.min(got.body.length, byteOffset + a)
      return ok(
        `# bytes ${start}..${end} of requestId ${requestId} (size ${got.body.length})\n\n${got.body.slice(start, end)}`
      )
    }
  )

  mcp.registerTool(
    'detect_bundler',
    {
      description:
        'Heuristically identify the bundler used to produce a captured script. Useful to decide whether deobfuscate_script will help (Webpack/Browserify yes, Vite/Rollup no).',
      inputSchema: {
        requestId: z.string()
      }
    },
    async ({ requestId }) => {
      const got = getScriptBody(requestId)
      if ('error' in got) return err(got.error)
      const result = detectBundler(got.body)
      return ok(JSON.stringify(result, null, 2))
    }
  )

  mcp.registerTool(
    'deobfuscate_script',
    {
      description:
        'Run the external `webcrack` CLI on a captured script and return the deobfuscated/unminified source. Only useful for Webpack/Browserify bundles — Vite/Rollup ESM output returns empty. Requires `webcrack` on PATH (npm i -g webcrack on Node 22).',
      inputSchema: {
        requestId: z.string()
      }
    },
    async ({ requestId }) => {
      const got = getScriptBody(requestId)
      if ('error' in got) return err(got.error)
      try {
        const out = await runWebcrack(got.body)
        if (!out.trim()) return ok('(webcrack returned empty output — likely a Vite/Rollup bundle)')
        return ok(out)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
