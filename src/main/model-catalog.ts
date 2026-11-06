import { spawn } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import path from 'node:path'

import { app, BrowserWindow } from 'electron'

import { detectAgents } from './acp-detect'
import { agentEnv } from './agent-env'
import { findClaudeCli } from './claude-cli'
import {
  claudeModelsFromInit,
  codexModelsFromList,
  parseCatalogFile,
  type AgentCatalog,
  type CatalogEntry,
  type CatalogFile
} from './model-catalog-parse'

// Live model lists, the way Orca does it: ask the user's own CLI (no prompt,
// no tokens), cache the answer on disk so the picker is instant on the next
// launch, and refresh in the background each launch so new models show up
// when the CLI updates. The hardcoded MODEL_CATALOG in the renderer is only
// the fallback when a probe has never succeeded.

const PROBE_TIMEOUT_MS = 30_000
const cacheFile = (): string => path.join(app.getPath('userData'), 'model-catalog.json')

let cache: CatalogFile | null = null
const inflight = new Map<string, Promise<AgentCatalog | null>>()

function load(): CatalogFile {
  if (!cache) {
    try {
      cache = parseCatalogFile(readFileSync(cacheFile(), 'utf8'))
    } catch {
      cache = { version: 1, agents: {} }
    }
  }
  return cache
}

/** One JSON-lines conversation with a short-lived CLI process; resolves with whatever `onLine` returns first. */
function converse<T>(
  command: string,
  args: string[],
  first: string,
  onLine: (msg: Record<string, any>, send: (o: unknown) => void) => T | undefined
): Promise<T> {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ['pipe', 'pipe', 'ignore'], env: agentEnv(command), shell: process.platform === 'win32' })
    const done = (fn: () => void): void => {
      clearTimeout(timer)
      child.kill()
      fn()
    }
    const timer = setTimeout(() => done(() => reject(new Error(`${path.basename(command)} model probe timed out`))), PROBE_TIMEOUT_MS)
    const send = (o: unknown): void => {
      child.stdin.write(JSON.stringify(o) + '\n')
    }
    let buf = ''
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', (chunk: string) => {
      buf += chunk
      let i: number
      while ((i = buf.indexOf('\n')) >= 0) {
        const raw = buf.slice(0, i).trim()
        buf = buf.slice(i + 1)
        if (!raw) continue
        try {
          const out = onLine(JSON.parse(raw), send)
          if (out !== undefined) return done(() => resolve(out))
        } catch (e) {
          return done(() => reject(e))
        }
      }
    })
    child.on('error', (e) => done(() => reject(e)))
    child.on('close', (code) => done(() => reject(new Error(`${path.basename(command)} exited (${code}) before listing models`))))
    child.stdin.write(first)
  })
}

async function probeClaude(): Promise<CatalogEntry[]> {
  const cli = findClaudeCli()
  if (!cli) throw new Error('claude CLI not found')
  const args = ['-p', '--input-format', 'stream-json', '--output-format', 'stream-json', '--verbose', '--setting-sources', '', '--strict-mcp-config']
  const req = JSON.stringify({ type: 'control_request', request_id: 'rever-models', request: { subtype: 'initialize' } }) + '\n'
  return converse(cli, args, req, (msg) => {
    if (msg.type !== 'control_response' || msg.response?.request_id !== 'rever-models') return undefined
    if (msg.response.subtype === 'error') throw new Error(String(msg.response.error))
    return claudeModelsFromInit(msg.response.response ?? {})
  })
}

async function probeCodex(): Promise<CatalogEntry[]> {
  const [found] = await detectAgents([{ command: 'codex' }])
  if (!found?.resolvedPath) throw new Error('codex CLI not found')
  const init = { id: 1, method: 'initialize', params: { clientInfo: { name: 'rever-browser', title: null, version: app.getVersion() }, capabilities: null } }
  const rows: CatalogEntry[] = []
  return converse(found.resolvedPath, ['app-server'], JSON.stringify(init) + '\n', (msg, send) => {
    if (msg.error) throw new Error(`codex app-server: ${msg.error.message ?? JSON.stringify(msg.error)}`)
    if (msg.id === 1) {
      send({ method: 'initialized' })
      send({ id: 2, method: 'model/list', params: {} })
      return undefined
    }
    if (typeof msg.id === 'number' && msg.id >= 2) {
      rows.push(...codexModelsFromList(msg.result ?? {}))
      const next = msg.result?.nextCursor
      // shortcut: stops after 5 pages; the list is a handful of models today.
      if (typeof next === 'string' && next && msg.id < 6) {
        send({ id: msg.id + 1, method: 'model/list', params: { cursor: next } })
        return undefined
      }
      return rows
    }
    return undefined
  })
}

const PROBES: Record<string, () => Promise<CatalogEntry[]>> = {
  'claude-code': probeClaude,
  codex: probeCodex
}

export function cachedCatalog(agentId: string): AgentCatalog | null {
  return load().agents[agentId] ?? null
}

/** Re-list one agent's models; concurrent calls share the probe. Failures keep the old cache. */
export function refreshCatalog(agentId: string): Promise<AgentCatalog | null> {
  const probe = PROBES[agentId]
  if (!probe) return Promise.resolve(null)
  const running = inflight.get(agentId)
  if (running) return running
  const p = probe()
    .then((models) => {
      if (models.length === 0) throw new Error('empty model list')
      const entry: AgentCatalog = { models, fetchedAt: new Date().toISOString() }
      const file = load()
      file.agents[agentId] = entry
      try {
        writeFileSync(cacheFile(), JSON.stringify(file, null, 1))
      } catch (e) {
        console.warn('[models] could not write cache:', e)
      }
      for (const w of BrowserWindow.getAllWindows()) {
        if (!w.isDestroyed()) w.webContents.send('models:updated', agentId, entry)
      }
      return entry
    })
    .catch((e) => {
      console.warn(`[models] ${agentId} probe failed:`, e instanceof Error ? e.message : e)
      return cachedCatalog(agentId)
    })
    .finally(() => inflight.delete(agentId))
  inflight.set(agentId, p)
  return p
}

/** Launch-time refresh for every agent we can list (background; never blocks startup). */
export function prewarmCatalogs(): void {
  for (const id of Object.keys(PROBES)) void refreshCatalog(id)
}
