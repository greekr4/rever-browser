#!/usr/bin/env node
// revb — script runner for a running Rever Browser. Zero dependencies (Node 18+).
// Spec: docs/rever-cli-design.md. Named `revb` because `rever` is taken on npm.
//
//   revb run <script.mjs | ->     run a script with every MCP tool as a global async fn
//   revb call <tool> ['<json>']   one call, mcp-call.py compatible
//   revb list                     tool names + one-line descriptions
//   revb skill [--full]           print the CLI guide (--full appends the live tool catalog)
//   revb endpoint                 print the published endpoint (diagnostics)

import { execSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const HERE = path.dirname(fileURLToPath(import.meta.url))
// ui_crawl / intruder_run / scan can outlive the 180s mcp-call.py allowed.
const CALL_TIMEOUT_MS = 600_000
const PAGE_CHARS = 60_000 // tool_result_more's max limit
const MAX_PAGES = 200 // 12M chars; the server keeps at most 20M across all results

// ---------------------------------------------------------------- pure helpers

/** Electron's userData for this app, computed the way src/main/index.ts does. */
export function userDataDir(env = process.env, platform = process.platform, home = os.homedir()) {
  if (env.REVER_USER_DATA_DIR) return env.REVER_USER_DATA_DIR
  const appData =
    platform === 'darwin'
      ? path.join(home, 'Library', 'Application Support')
      : platform === 'win32'
        ? env.APPDATA || path.join(home, 'AppData', 'Roaming')
        : env.XDG_CONFIG_HOME || path.join(home, '.config')
  return path.join(appData, 'rever-browser')
}

/**
 * Pick the JSON-RPC message with `id` out of a response body. The body is either
 * plain JSON or an SSE stream that may carry notifications before the reply, so
 * the first `data:` line is not necessarily ours.
 */
export function pickRpcResponse(raw, id) {
  const text = raw.trim()
  if (!text) return null
  if (text.startsWith('{') || text.startsWith('[')) {
    const msg = JSON.parse(text)
    const all = Array.isArray(msg) ? msg : [msg]
    return all.find((m) => m.id === id) ?? null
  }
  for (const event of text.split(/\r?\n\r?\n/)) {
    const data = event
      .split(/\r?\n/)
      .filter((l) => l.startsWith('data:'))
      .map((l) => l.slice(5).replace(/^ /, ''))
      .join('\n')
    if (!data) continue
    let msg
    try {
      msg = JSON.parse(data)
    } catch {
      continue
    }
    if (msg.id === id) return msg
  }
  return null
}

/** Text → parsed JSON when it is JSON, otherwise the string itself. */
export function toValue(text) {
  const t = text.trim()
  if (!/^[[{"]|^-?\d|^(true|false|null)$/.test(t)) return text
  try {
    return JSON.parse(t)
  } catch {
    return text
  }
}

const TRUNC_RE =
  /\n\n\[truncated: showing (\d+) of (\d+) chars\. Call tool_result_more with resultId="([^"]+)", offset=(\d+) for the rest\.\]$/
const MORE_RE = /\n\n\[more: call tool_result_more resultId="[^"]+", offset=(\d+) \(\d+ chars left\)\]$/
const END_RE = /\n\n\[end of result: \d+ chars total\]$/

/** Split okBudgeted's truncation footer (src/main/mcp/utils.ts) off a result. */
export function parseTruncated(text) {
  const m = TRUNC_RE.exec(text)
  if (!m) return null
  return { head: text.slice(0, m.index), total: Number(m[2]), resultId: m[3], offset: Number(m[4]) }
}

/** Split tool_result_more's footer (src/main/mcp/tools/paging.ts) off a page. */
export function parsePage(text) {
  const more = MORE_RE.exec(text)
  if (more) return { slice: text.slice(0, more.index), nextOffset: Number(more[1]) }
  const end = END_RE.exec(text)
  if (end) return { slice: text.slice(0, end.index), nextOffset: null }
  throw new Error('unexpected tool_result_more reply: ' + text.slice(-200))
}

/**
 * Follow a truncation footer to the end. `more(resultId, offset)` returns the
 * raw page text. Stops after `maxPages` and leaves a footer so nothing is lost
 * silently.
 */
export async function fetchRest(first, more, maxPages = MAX_PAGES) {
  const t = parseTruncated(first)
  if (!t) return first
  let out = t.head
  let offset = t.offset
  for (let i = 0; i < maxPages; i++) {
    const page = parsePage(await more(t.resultId, offset))
    out += page.slice
    if (page.nextOffset == null) return out
    offset = page.nextOffset
  }
  return (
    out +
    `\n\n[truncated: revb stopped after ${maxPages} pages at ${offset} of ${t.total} chars. ` +
    `Call tool_result_more with resultId="${t.resultId}", offset=${offset} for the rest.]`
  )
}

// ---------------------------------------------------------------- endpoint

export function readEndpoint() {
  const file = path.join(userDataDir(), 'mcp-endpoint.json')
  if (!existsSync(file)) {
    fail(`Rever Browser is not running (no ${file}). Start the app, then retry.`)
  }
  const ep = JSON.parse(readFileSync(file, 'utf8'))
  // The app never removes the file on exit, so a stale one points at a dead port.
  if (ep.pid) {
    try {
      process.kill(ep.pid, 0)
    } catch (e) {
      if (e.code === 'ESRCH') fail(`Rever Browser is not running (pid ${ep.pid} from ${file} is gone). Start the app, then retry.`)
    }
  }
  const host = new URL(ep.url).hostname
  if (host !== '127.0.0.1' && host !== 'localhost') {
    fail(`refusing non-loopback endpoint ${ep.url} in ${file}`)
  }
  return { ...ep, file }
}

// ---------------------------------------------------------------- client

class RpcError extends Error {}

export class Client {
  constructor(url, token) {
    this.url = new URL(url)
    this.token = token
    this.sid = null
    this.nextId = 1
  }

  request(method, payload) {
    return new Promise((resolve, reject) => {
      const body = payload == null ? null : JSON.stringify(payload)
      const req = http.request(
        this.url,
        {
          method,
          agent: false, // a keep-alive socket would hold the process open after the script ends
          headers: {
            'Content-Type': 'application/json',
            Accept: 'application/json, text/event-stream',
            ...(this.token ? { Authorization: `Bearer ${this.token}` } : {}),
            // Every tool, not just the app's current Tools→Access profile.
            'X-Rever-Catalog': 'full',
            ...(this.sid ? { 'mcp-session-id': this.sid } : {})
          },
          timeout: CALL_TIMEOUT_MS
        },
        (res) => {
          const chunks = []
          res.on('data', (c) => chunks.push(c))
          res.on('end', () => {
            const text = Buffer.concat(chunks).toString('utf8')
            if (res.statusCode >= 400) {
              reject(new Error(`HTTP ${res.statusCode} from ${this.url}: ${text.slice(0, 300)}`))
            } else {
              if (!this.sid && res.headers['mcp-session-id']) this.sid = res.headers['mcp-session-id']
              resolve(text) // 202 / empty body for notifications → ''
            }
          })
          res.on('error', reject)
        }
      )
      req.on('timeout', () => req.destroy(new Error(`no reply within ${CALL_TIMEOUT_MS / 1000}s`)))
      req.on('error', reject)
      req.end(body ?? undefined)
    })
  }

  async rpc(method, params) {
    const id = this.nextId++
    const raw = await this.request('POST', { jsonrpc: '2.0', id, method, params })
    const msg = pickRpcResponse(raw, id)
    if (!msg) throw new Error(`no JSON-RPC reply for ${method} (id ${id})`)
    if (msg.error) throw new RpcError(`${method}: ${msg.error.message} (code ${msg.error.code})`)
    return msg.result
  }

  async connect() {
    const res = await this.rpc('initialize', {
      protocolVersion: '2025-03-26',
      capabilities: {},
      clientInfo: { name: 'revb', version: cliVersion() }
    })
    await this.request('POST', { jsonrpc: '2.0', method: 'notifications/initialized' })
    this.serverVersion = res?.serverInfo?.version
    return this
  }

  async tools() {
    // shortcut: ignores nextCursor — McpServer returns every tool in one page; loop if that changes.
    return (await this.rpc('tools/list', {})).tools
  }

  /** Raw tools/call; throws on JSON-RPC error and isError. */
  async callRaw(name, args = {}) {
    const res = await this.rpc('tools/call', { name, arguments: args })
    const texts = (res.content ?? []).filter((c) => c.type === 'text').map((c) => c.text)
    if (res.isError) throw new Error(withHint(`${name}: ${texts.join('\n') || 'tool error'}`))
    return { res, texts }
  }

  /** The script-facing call: pages, parses, saves images. */
  async call(name, args = {}) {
    const { res, texts } = await this.callRaw(name, args)
    const images = (res.content ?? []).filter((c) => c.type === 'image').map(saveImage)
    // Extra text parts are page notes (scope warnings, answered dialogs, downloads)
    // appended by server.ts — stderr keeps stdout parseable.
    for (const note of texts.slice(1)) process.stderr.write(`[revb] ${name}: ${note}\n`)
    if (texts.length === 0) return images.length === 1 ? images[0] : images
    for (const p of images) process.stderr.write(`[revb] ${name}: image saved ${p}\n`)
    const full = await fetchRest(texts[0], async (resultId, offset) =>
      (await this.callRaw('tool_result_more', { resultId, offset, limit: PAGE_CHARS })).texts[0]
    )
    return toValue(full)
  }

  async close() {
    // Sessions have no TTL on the server; leaving one open leaks it until the app quits.
    if (this.sid) await this.request('DELETE', null).catch(() => {})
    this.sid = null
  }
}

function withHint(msg) {
  if (/no active browser target/.test(msg)) {
    msg += '\n  hint: the app is up but no tab is attached yet — wait until the first page has loaded, then retry.'
  }
  if (/waits for the user/.test(msg)) {
    msg += '\n  hint: a page dialog blocks every tool; answer it in the app window first.'
  }
  return msg
}

function saveImage(part) {
  const ext = { 'image/png': 'png', 'image/jpeg': 'jpg' }[part.mimeType] ?? 'bin'
  const dir = process.env.REVER_SHOT_DIR || os.tmpdir()
  mkdirSync(dir, { recursive: true })
  const file = path.join(dir, `rever-shot-${Date.now()}-${Math.random().toString(36).slice(2, 6)}.${ext}`)
  writeFileSync(file, Buffer.from(part.data ?? '', 'base64'))
  return file
}

// ---------------------------------------------------------------- commands

function cliVersion() {
  try {
    return JSON.parse(readFileSync(path.join(HERE, '..', 'package.json'), 'utf8')).version
  } catch {
    return '0.0.0'
  }
}

function warnVersion(server) {
  const mm = (v) => String(v ?? '').split('.').slice(0, 2).join('.')
  if (server && mm(server) !== mm(cliVersion())) {
    process.stderr.write(
      `[revb] warning: app is ${server}, this CLI/guide is ${cliVersion()} — tool names in the guide may differ. Check \`revb list\`.\n`
    )
  }
}

async function withClient(fn) {
  const ep = readEndpoint()
  const client = new Client(ep.url, ep.token)
  const onSignal = () => client.close().finally(() => process.exit(130))
  process.once('SIGINT', onSignal)
  process.once('SIGTERM', onSignal)
  try {
    await client.connect()
    warnVersion(client.serverVersion)
    return await fn(client)
  } finally {
    await client.close()
  }
}

function print(v) {
  process.stdout.write((typeof v === 'string' ? v : JSON.stringify(v, null, 2)) + '\n')
}

async function cmdRun(file) {
  const code = !file || file === '-' ? readFileSync(0, 'utf8') : null
  await withClient(async (client) => {
    const tools = await client.tools()
    for (const t of tools) {
      // Never shadow a real global (fetch, eval…) — rev.call still reaches it.
      if (!(t.name in globalThis)) globalThis[t.name] = (args) => client.call(t.name, args)
    }
    globalThis.rev = {
      tools: () => tools.map((t) => ({ name: t.name, description: t.description })),
      call: (name, args) => client.call(name, args),
      endpoint: client.url.href
    }
    globalThis.sh = (cmd, input) =>
      execSync(cmd, { encoding: 'utf8', input, stdio: [input == null ? 'ignore' : 'pipe', 'pipe', 'inherit'] })

    let target
    if (code != null) {
      // stdin → temp .mjs so top-level await and node: imports work.
      target = path.join(os.tmpdir(), `revb-stdin-${process.pid}.mjs`)
      writeFileSync(target, code)
    } else {
      target = path.resolve(file)
    }
    try {
      await import(pathToFileURL(target).href)
    } finally {
      if (code != null) rmSync(target, { force: true })
    }
  })
}

async function cmdSkill(full) {
  print(readFileSync(path.join(HERE, '..', 'skills', 'rever', 'cli.md'), 'utf8'))
  if (!full) return
  await withClient(async (client) => {
    const tools = await client.tools()
    print(`\n## Live tool catalog (${tools.length} tools, app ${client.serverVersion})\n`)
    for (const t of tools.sort((a, b) => a.name.localeCompare(b.name))) {
      print(`- \`${t.name}\` — ${firstSentence(t.description)}`)
    }
  })
}

function firstSentence(d = '') {
  const s = d.split(/(?<=\.)\s|\n/)[0]
  return s.length > 160 ? s.slice(0, 157) + '…' : s
}

const USAGE = `revb — drive a running Rever Browser from scripts

  revb run <script.mjs | ->     run a script; every MCP tool is a global async fn
  revb call <tool> ['<json>']   call one tool and print its result
  revb list                     list tools
  revb skill [--full]           print the scripting guide (--full: + live tool catalog)
  revb endpoint                 print the endpoint the CLI connects to`

function fail(msg) {
  process.stderr.write(`revb: ${msg}\n`)
  process.exit(1)
}

async function main(argv) {
  const [cmd, ...rest] = argv
  switch (cmd) {
    case 'run':
      return cmdRun(rest[0])
    case 'call': {
      if (!rest[0]) fail('usage: revb call <tool> [\'<json args>\']')
      const args = rest[1] ? JSON.parse(rest[1]) : {}
      return withClient(async (c) => print(await c.call(rest[0], args)))
    }
    case 'list':
      return withClient(async (c) => {
        const tools = (await c.tools()).sort((a, b) => a.name.localeCompare(b.name))
        for (const t of tools) print(`${t.name}\t${firstSentence(t.description)}`)
      })
    case 'skill':
      return cmdSkill(rest.includes('--full'))
    case 'endpoint':
      return print(readEndpoint())
    default:
      print(USAGE)
      if (cmd && cmd !== '-h' && cmd !== '--help') process.exit(1)
  }
}

if (process.argv[1] && realpathSync(fileURLToPath(import.meta.url)) === realpathSync(process.argv[1])) {
  main(process.argv.slice(2)).catch((e) => {
    process.stderr.write(`revb: ${e?.message ?? e}\n`)
    process.exit(1)
  })
}
