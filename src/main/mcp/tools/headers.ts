import { z } from 'zod'
import { randomUUID } from 'node:crypto'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import {
  getInterceptRules,
  setInterceptRules,
  applyFetchIntercept,
  type InterceptRule
} from '../../chrome-cdp'
import { ok, err, errorMessage } from '../utils'

interface HeaderPreset {
  id: string
  name: string
  headers: Record<string, string>
  urlPattern: string
  intercepRuleId?: string
}

const presets = new Map<string, HeaderPreset>()

const BUILT_IN_PRESETS: Omit<HeaderPreset, 'id'>[] = [
  {
    name: 'localhost-trust',
    urlPattern: '*',
    headers: { 'X-Forwarded-For': '127.0.0.1', 'X-Real-IP': '127.0.0.1', 'X-Originating-IP': '127.0.0.1' }
  },
  {
    name: 'mobile-safari-ua',
    urlPattern: '*',
    headers: {
      'User-Agent':
        'Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1'
    }
  },
  {
    name: 'googlebot',
    urlPattern: '*',
    headers: {
      'User-Agent':
        'Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)'
    }
  },
  {
    name: 'referer-trust',
    urlPattern: '*',
    headers: { Referer: 'https://www.google.com/' }
  }
]

function ensureBuiltins() {
  if (presets.size > 0) return
  for (const p of BUILT_IN_PRESETS) presets.set(p.name, { id: randomUUID(), ...p })
}

export function registerHeaderTools(mcp: McpServer) {
  ensureBuiltins()

  mcp.registerTool(
    'header_preset_list',
    {
      description:
        'List saved header presets (X-Forwarded-For, custom UA, Referer, Authorization). Built-ins ship with the app.'
    },
    async () => {
      return ok(
        JSON.stringify(
          [...presets.values()].map((p) => ({
            id: p.id,
            name: p.name,
            urlPattern: p.urlPattern,
            headers: p.headers,
            active: !!p.intercepRuleId
          })),
          null,
          2
        )
      )
    }
  )

  mcp.registerTool(
    'header_preset_save',
    {
      description: 'Save a new header preset.',
      inputSchema: {
        name: z.string(),
        headers: z.record(z.string(), z.string()),
        urlPattern: z.string().optional().describe('Default *')
      }
    },
    async ({ name, headers, urlPattern = '*' }) => {
      const preset: HeaderPreset = { id: randomUUID(), name, headers, urlPattern }
      presets.set(name, preset)
      return ok(JSON.stringify({ name, id: preset.id }, null, 2))
    }
  )

  mcp.registerTool(
    'header_preset_apply',
    {
      description:
        'Activate a header preset — registers an intercept rule that injects the preset\'s headers into every matching outbound request.',
      inputSchema: { name: z.string() }
    },
    async ({ name }) => {
      try {
        const preset = presets.get(name)
        if (!preset) return err(`unknown preset: ${name}`)
        if (preset.intercepRuleId) return ok(`already active: ${name}`)
        const rule: InterceptRule = {
          id: randomUUID(),
          urlPattern: preset.urlPattern,
          mode: 'modify',
          stage: 'Request',
          modifyHeaders: preset.headers
        }
        setInterceptRules([...getInterceptRules(), rule])
        preset.intercepRuleId = rule.id
        await applyFetchIntercept()
        return ok(`activated ${name} → rule ${rule.id}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'header_preset_disable',
    {
      description: 'Deactivate a header preset (removes the intercept rule).',
      inputSchema: { name: z.string() }
    },
    async ({ name }) => {
      try {
        const preset = presets.get(name)
        if (!preset?.intercepRuleId) return err(`not active: ${name}`)
        setInterceptRules(getInterceptRules().filter((r) => r.id !== preset.intercepRuleId))
        preset.intercepRuleId = undefined
        await applyFetchIntercept()
        return ok(`deactivated ${name}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
