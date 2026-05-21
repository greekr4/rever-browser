import { z } from 'zod'
import { randomUUID } from 'node:crypto'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { ok, err } from '../utils'

interface Finding {
  id: string
  title: string
  severity: 'info' | 'low' | 'medium' | 'high' | 'critical'
  body: string
  requestId?: string
  tags: string[]
  createdAt: number
}

const findings = new Map<string, Finding>()

export function listFindings(): Finding[] {
  return [...findings.values()].sort((a, b) => b.createdAt - a.createdAt)
}

export function registerFindingTools(mcp: McpServer) {
  mcp.registerTool(
    'finding_add',
    {
      description:
        'Save a finding/evidence note. Use during reversing to bookmark interesting requests, payloads, or observations. Markdown body supported.',
      inputSchema: {
        title: z.string().describe('Short title'),
        body: z.string().describe('Markdown body — observations, payload, screenshot link, etc.'),
        severity: z.enum(['info', 'low', 'medium', 'high', 'critical']).optional(),
        requestId: z.string().optional().describe('Optional traffic-store requestId reference'),
        tags: z.array(z.string()).optional()
      }
    },
    async ({ title, body, severity = 'info', requestId, tags = [] }) => {
      const f: Finding = {
        id: randomUUID(),
        title,
        severity,
        body,
        requestId,
        tags,
        createdAt: Date.now()
      }
      findings.set(f.id, f)
      return ok(JSON.stringify({ id: f.id, title, severity }, null, 2))
    }
  )

  mcp.registerTool(
    'finding_list',
    {
      description: 'List saved findings (newest first).',
      inputSchema: {
        tag: z.string().optional().describe('Filter by tag'),
        severity: z.enum(['info', 'low', 'medium', 'high', 'critical']).optional()
      }
    },
    async ({ tag, severity }) => {
      let list = listFindings()
      if (tag) list = list.filter((f) => f.tags.includes(tag))
      if (severity) list = list.filter((f) => f.severity === severity)
      return ok(JSON.stringify(list, null, 2))
    }
  )

  mcp.registerTool(
    'finding_remove',
    {
      description: 'Delete a finding by ID.',
      inputSchema: { id: z.string() }
    },
    async ({ id }) => {
      if (!findings.has(id)) return err(`unknown finding: ${id}`)
      findings.delete(id)
      return ok(`removed ${id}`)
    }
  )
}
