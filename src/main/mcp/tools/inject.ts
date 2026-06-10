import { z } from 'zod'
import { readFileSync, writeFileSync, existsSync } from 'node:fs'
import { join } from 'node:path'
import { randomUUID } from 'node:crypto'

import { app } from 'electron'
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getActiveTarget, setLoadInjectionsHook } from '../../chrome-cdp'
import { ok, err, errorMessage } from '../utils'
import type { Debugger, WebContents } from 'electron'

interface Snippet {
  id: string
  name: string
  hostGlob: string
  code: string
  enabled: boolean
}

// Per-target scriptIdentifier map: (snippetId -> scriptIdentifier)
const targetScriptIds = new Map<string, Map<string, string>>()

function getSnippetsPath(): string {
  return join(app.getPath('userData'), 'injected-snippets.json')
}

function loadSnippets(): Snippet[] {
  const path = getSnippetsPath()
  if (!existsSync(path)) return []
  try {
    return JSON.parse(readFileSync(path, 'utf8')) as Snippet[]
  } catch {
    return []
  }
}

function saveSnippets(snippets: Snippet[]): void {
  writeFileSync(getSnippetsPath(), JSON.stringify(snippets, null, 2), 'utf8')
}

function globToRegex(glob: string): RegExp {
  const escaped = glob
    .replace(/[.+^${}()|[\]\\]/g, '\\$&')
    .replace(/\*/g, '.*')
  return new RegExp(`^${escaped}$`)
}

function hostMatches(hostGlob: string, host: string): boolean {
  return globToRegex(hostGlob).test(host)
}

function getCurrentHost(): string {
  const target = getActiveTarget()
  if (!target) return ''
  try {
    const url = target.wc.getURL()
    return new URL(url).host
  } catch {
    return ''
  }
}

export async function loadAndApplyInjections(target: { dbg: Debugger; wc: WebContents }): Promise<void> {
  const snippets = loadSnippets()
  let host = ''
  try {
    host = new URL(target.wc.getURL()).host
  } catch {}

  const wcId = target.wc.id.toString()
  if (!targetScriptIds.has(wcId)) {
    targetScriptIds.set(wcId, new Map())
  }
  const idMap = targetScriptIds.get(wcId)!

  for (const s of snippets) {
    if (!s.enabled) continue
    if (!hostMatches(s.hostGlob, host)) continue
    try {
      // 이전 등록이 있으면 먼저 제거 (재부착 시 중복 누수 방지)
      const prevIdentifier = idMap.get(s.id)
      if (prevIdentifier) {
        await target.dbg
          .sendCommand('Page.removeScriptToEvaluateOnNewDocument', { identifier: prevIdentifier })
          .catch(() => {})
        idMap.delete(s.id)
      }
      const res = (await target.dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', {
        source: s.code
      })) as { identifier: string }
      idMap.set(s.id, res.identifier)
    } catch (e) {
      console.error('[inject] failed to register snippet', s.id, e)
    }
  }
}

export function registerInjectTools(mcp: McpServer) {
  // Register the hook into chrome-cdp
  setLoadInjectionsHook(loadAndApplyInjections)

  mcp.registerTool(
    'inject_add',
    {
      description:
        'Add a persistent JavaScript snippet that will be injected on every page load for matching hosts. Optionally registers immediately on the current page.',
      inputSchema: {
        name: z.string().describe('Human-readable name for this snippet'),
        hostGlob: z.string().describe('Host glob pattern, e.g. "*.example.com" or "api.example.com"'),
        code: z.string().describe('JavaScript code to inject'),
        enabled: z.boolean().optional().describe('Whether to enable immediately (default true)')
      }
    },
    async ({ name, hostGlob, code, enabled = true }) => {
      const snippets = loadSnippets()
      const snippet: Snippet = { id: randomUUID(), name, hostGlob, code, enabled }
      snippets.push(snippet)
      saveSnippets(snippets)

      // Register immediately if current host matches
      if (enabled) {
        const host = getCurrentHost()
        if (host && hostMatches(hostGlob, host)) {
          const target = getActiveTarget()
          if (target) {
            try {
              const res = (await target.dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', {
                source: code
              })) as { identifier: string }
              const wcId = target.wc.id.toString()
              if (!targetScriptIds.has(wcId)) targetScriptIds.set(wcId, new Map())
              targetScriptIds.get(wcId)!.set(snippet.id, res.identifier)
            } catch (e) {
              console.error('[inject] immediate register failed:', e)
            }
          }
        }
      }

      return ok(JSON.stringify({ id: snippet.id, name, hostGlob, enabled }, null, 2))
    }
  )

  mcp.registerTool(
    'inject_list',
    {
      description: 'List all saved injection snippets.'
    },
    async () => {
      const snippets = loadSnippets()
      return ok(JSON.stringify(snippets.map(({ id, name, hostGlob, enabled }) => ({ id, name, hostGlob, enabled })), null, 2))
    }
  )

  mcp.registerTool(
    'inject_remove',
    {
      description: 'Remove an injection snippet by ID.',
      inputSchema: {
        id: z.string().describe('Snippet ID from inject_list')
      }
    },
    async ({ id }) => {
      const snippets = loadSnippets()
      const idx = snippets.findIndex((s) => s.id === id)
      if (idx === -1) return err(`unknown snippet id: ${id}`)
      snippets.splice(idx, 1)
      saveSnippets(snippets)

      // Remove from current target if registered
      const target = getActiveTarget()
      if (target) {
        const wcId = target.wc.id.toString()
        const idMap = targetScriptIds.get(wcId)
        const scriptId = idMap?.get(id)
        if (scriptId) {
          await target.dbg
            .sendCommand('Page.removeScriptToEvaluateOnNewDocument', { identifier: scriptId })
            .catch(() => {})
          idMap?.delete(id)
        }
      }

      return ok(`snippet ${id} removed`)
    }
  )

  mcp.registerTool(
    'inject_toggle',
    {
      description: 'Enable or disable an injection snippet.',
      inputSchema: {
        id: z.string().describe('Snippet ID from inject_list'),
        enabled: z.boolean().describe('New enabled state')
      }
    },
    async ({ id, enabled }) => {
      const snippets = loadSnippets()
      const s = snippets.find((x) => x.id === id)
      if (!s) return err(`unknown snippet id: ${id}`)
      s.enabled = enabled
      saveSnippets(snippets)
      return ok(`snippet ${id} ${enabled ? 'enabled' : 'disabled'}`)
    }
  )

  mcp.registerTool(
    'inject_run_now',
    {
      description: 'Run a one-shot JavaScript snippet in the current page immediately (not persisted).',
      inputSchema: {
        code: z.string().describe('JavaScript code to run now')
      }
    },
    async ({ code }) => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        const res = (await target.dbg.sendCommand('Runtime.evaluate', {
          expression: code,
          returnByValue: true,
          awaitPromise: true
        })) as { result: { value?: unknown }; exceptionDetails?: { text: string } }
        if (res.exceptionDetails) return err(res.exceptionDetails.text)
        return ok(res.result.value != null ? JSON.stringify(res.result.value) : 'undefined')
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
