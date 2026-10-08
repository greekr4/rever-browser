import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getActiveTarget } from '../../chrome-cdp'
import { addScopeHosts, clearScope, getScope, hostnameOf, setScope } from '../scope'
import { ok, err } from '../utils'

function currentTargetHost(): string | null {
  const target = getActiveTarget()
  if (!target) return null
  try {
    return hostnameOf(target.wc.getURL()) ?? null
  } catch {
    return null
  }
}

function scopeState() {
  const { domains, enabled } = getScope()
  return { enabled, domains, note: enabled ? undefined : 'No scope set — all hosts allowed.' }
}

export function registerScopeTools(mcp: McpServer) {
  mcp.registerTool(
    'scope_status',
    {
      description:
        'Show the active target scope. When enabled, navigate/replay/repeater/intruder/burst refuse hosts outside these registrable domains. Empty = no enforcement (all hosts allowed).'
    },
    async () => ok(JSON.stringify(scopeState(), null, 2))
  )

  mcp.registerTool(
    'scope_set',
    {
      description:
        'Set the target scope to the given hosts/URLs (stored as registrable domains, so subdomains like api./cdn. are included). Pass no hosts to scope to the current tab. Replaces any existing scope.',
      inputSchema: {
        hosts: z
          .array(z.string())
          .optional()
          .describe('Hosts or URLs. Omit to use the current tab host.')
      }
    },
    async ({ hosts }) => {
      let input = hosts ?? []
      if (input.length === 0) {
        const host = currentTargetHost()
        if (!host) return err('no hosts given and no active tab to derive scope from')
        input = [host]
      }
      const domains = setScope(input)
      if (domains.length === 0) return err('no valid hosts parsed from input')
      return ok(JSON.stringify(scopeState(), null, 2))
    }
  )

  mcp.registerTool(
    'scope_add',
    {
      description: 'Add hosts/URLs to the current scope (registrable domains).',
      inputSchema: {
        hosts: z.array(z.string()).min(1).describe('Hosts or URLs to allow')
      }
    },
    async ({ hosts }) => {
      addScopeHosts(hosts)
      return ok(JSON.stringify(scopeState(), null, 2))
    }
  )

  mcp.registerTool(
    'scope_clear',
    {
      description: 'Clear the scope (disables enforcement — all hosts allowed again).'
    },
    async () => {
      clearScope()
      return ok(JSON.stringify(scopeState(), null, 2))
    }
  )
}
