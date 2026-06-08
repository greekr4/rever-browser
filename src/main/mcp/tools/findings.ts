import { z } from 'zod'
import { randomUUID } from 'node:crypto'
import { readFileSync, writeFileSync, existsSync } from 'node:fs'
import path from 'node:path'

import { app } from 'electron'
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { ok, err } from '../utils'

type Severity = 'info' | 'low' | 'medium' | 'high' | 'critical'
type Category = 'endpoint' | 'auth' | 'vuln' | 'secret' | 'other'

interface Finding {
  id: string
  title: string
  severity: Severity
  category: Category
  body: string
  requestId?: string
  tags: string[]
  createdAt: number
}

const SEVERITY_RANK: Record<Severity, number> = { critical: 5, high: 4, medium: 3, low: 2, info: 1 }
const SEVERITY_LABEL: Record<Severity, string> = {
  critical: '🔴 critical',
  high: '🟠 high',
  medium: '🟡 medium',
  low: '🟢 low',
  info: 'ℹ️ info'
}
const CATEGORY_ORDER: Category[] = ['endpoint', 'auth', 'vuln', 'secret', 'other']
const CATEGORY_LABEL: Record<Category, string> = {
  endpoint: 'API Endpoints',
  auth: 'Auth Flow',
  vuln: 'Vulnerabilities',
  secret: 'Secrets / Keys',
  other: 'Other'
}

const STORE_FILE = 'findings.json'
const findings = new Map<string, Finding>()
let loaded = false

function storePath(): string {
  return path.join(app.getPath('userData'), STORE_FILE)
}

function load(): void {
  if (loaded) return
  loaded = true
  try {
    if (!existsSync(storePath())) return
    const arr: Finding[] = JSON.parse(readFileSync(storePath(), 'utf8'))
    for (const f of arr) {
      // Tolerate findings written before `category` existed.
      findings.set(f.id, { ...f, category: f.category ?? 'other', tags: f.tags ?? [] })
    }
  } catch (e) {
    console.error('[findings] load failed:', e)
  }
}

function save(): void {
  try {
    writeFileSync(storePath(), JSON.stringify([...findings.values()], null, 2))
  } catch (e) {
    console.error('[findings] save failed:', e)
  }
}

export function listFindings(): Finding[] {
  return [...findings.values()].sort((a, b) => b.createdAt - a.createdAt)
}

function renderMarkdown(list: Finding[]): string {
  const lines: string[] = []
  lines.push('# Reversing Session Findings', '')
  lines.push(`_${list.length} finding${list.length === 1 ? '' : 's'}_`, '')

  for (const cat of CATEGORY_ORDER) {
    const inCat = list
      .filter((f) => f.category === cat)
      .sort((a, b) => SEVERITY_RANK[b.severity] - SEVERITY_RANK[a.severity] || b.createdAt - a.createdAt)
    if (inCat.length === 0) continue
    lines.push(`## ${CATEGORY_LABEL[cat]} (${inCat.length})`, '')
    for (const f of inCat) {
      lines.push(`### ${f.title}`)
      const meta = [SEVERITY_LABEL[f.severity]]
      if (f.tags.length) meta.push(`tags: ${f.tags.join(', ')}`)
      if (f.requestId) meta.push(`request: \`${f.requestId}\``)
      lines.push(`_${meta.join(' · ')}_`, '')
      lines.push(f.body.trim(), '')
    }
  }
  return lines.join('\n')
}

export function registerFindingTools(mcp: McpServer) {
  load()

  mcp.registerTool(
    'finding_add',
    {
      description:
        'Save a finding/evidence note as a durable session artifact (persisted to disk). Use during reversing to bookmark endpoints, auth flows, secrets, or vulnerabilities. Markdown body supported.',
      inputSchema: {
        title: z.string().describe('Short title'),
        body: z.string().describe('Markdown body — observations, payload, repro, etc.'),
        category: z
          .enum(['endpoint', 'auth', 'vuln', 'secret', 'other'])
          .optional()
          .describe('What kind of finding this is (groups the export report)'),
        severity: z.enum(['info', 'low', 'medium', 'high', 'critical']).optional(),
        requestId: z.string().optional().describe('Optional traffic-store requestId reference'),
        tags: z.array(z.string()).optional()
      }
    },
    async ({ title, body, category = 'other', severity = 'info', requestId, tags = [] }) => {
      const f: Finding = {
        id: randomUUID(),
        title,
        severity,
        category,
        body,
        requestId,
        tags,
        createdAt: Date.now()
      }
      findings.set(f.id, f)
      save()
      return ok(JSON.stringify({ id: f.id, title, category, severity }, null, 2))
    }
  )

  mcp.registerTool(
    'finding_list',
    {
      description: 'List saved findings (newest first).',
      inputSchema: {
        category: z.enum(['endpoint', 'auth', 'vuln', 'secret', 'other']).optional(),
        tag: z.string().optional().describe('Filter by tag'),
        severity: z.enum(['info', 'low', 'medium', 'high', 'critical']).optional()
      }
    },
    async ({ category, tag, severity }) => {
      let list = listFindings()
      if (category) list = list.filter((f) => f.category === category)
      if (tag) list = list.filter((f) => f.tags.includes(tag))
      if (severity) list = list.filter((f) => f.severity === severity)
      return ok(JSON.stringify(list, null, 2))
    }
  )

  mcp.registerTool(
    'finding_export',
    {
      description:
        'Render all findings as a structured Markdown report grouped by category (endpoints / auth / vulns / secrets) and ordered by severity. The session deliverable.',
      inputSchema: {}
    },
    async () => {
      const list = listFindings()
      if (list.length === 0) return ok('No findings recorded yet.')
      return ok(renderMarkdown(list))
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
      save()
      return ok(`removed ${id}`)
    }
  )
}
