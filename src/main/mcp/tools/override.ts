import { z } from 'zod'
import { randomUUID } from 'node:crypto'
import { readFileSync } from 'node:fs'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import {
  getInterceptRules,
  setInterceptRules,
  applyFetchIntercept,
  type InterceptRule
} from '../../chrome-cdp'
import { ok, err, errorMessage } from '../utils'

// Local store of override rule IDs so we can list "just overrides" separately
// from other intercept rules.
const overrideIds = new Set<string>()

export function registerOverrideTools(mcp: McpServer) {
  mcp.registerTool(
    'override_add',
    {
      description:
        'Register a local override: when a URL matching `urlPattern` is requested, fulfill it with `body` (or contents of `file`) instead of hitting the network. Equivalent to DevTools "Local Overrides". Use to neutralize anti-bot scripts, replace a bundle with a beautified copy, or patch a comparison.',
      inputSchema: {
        urlPattern: z.string().describe('CDP wildcard pattern, e.g. "*/static/main.js"'),
        body: z.string().optional().describe('Inline replacement body'),
        file: z.string().optional().describe('Local file path to load as the body (mutually exclusive with body)'),
        mimeType: z.string().optional()
      }
    },
    async ({ urlPattern, body, file, mimeType }) => {
      try {
        if (body == null && !file) return err('provide body or file')
        const text = file ? readFileSync(file, 'utf8') : (body ?? '')
        const rule: InterceptRule = {
          id: randomUUID(),
          urlPattern,
          mode: 'modify',
          stage: 'Request',
          replaceBody: text,
          ...(mimeType ? { modifyHeaders: { 'content-type': mimeType } } : {})
        }
        setInterceptRules([...getInterceptRules(), rule])
        overrideIds.add(rule.id)
        await applyFetchIntercept()
        return ok(JSON.stringify({ id: rule.id, urlPattern, bytes: text.length }, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'override_list',
    { description: 'List active local overrides.' },
    async () => {
      const rules = getInterceptRules().filter((r) => overrideIds.has(r.id))
      return ok(
        JSON.stringify(
          rules.map((r) => ({
            id: r.id,
            urlPattern: r.urlPattern,
            bytes: r.replaceBody?.length ?? 0,
            mimeType: r.modifyHeaders?.['content-type']
          })),
          null,
          2
        )
      )
    }
  )

  mcp.registerTool(
    'override_remove',
    {
      description: 'Remove a local override by ID.',
      inputSchema: { id: z.string() }
    },
    async ({ id }) => {
      if (!overrideIds.has(id)) return err(`not an override: ${id}`)
      setInterceptRules(getInterceptRules().filter((r) => r.id !== id))
      overrideIds.delete(id)
      await applyFetchIntercept()
      return ok(`removed ${id}`)
    }
  )
}
