#!/usr/bin/env node
// revb — script runner for a running Rever Browser. Zero dependencies (Node 18+).
// Spec: docs/rever-cli-design.md. Named `revb` because `rever` is taken on npm.
//
//   revb run <script.mjs | ->          run a script with every MCP tool as a global async fn
//   revb call <tool> ['<json>' | --k v] one call: a JSON object, or flags typed by the tool's schema
//   revb describe <tool>               one tool's description and input schema, as JSON
//   revb md <url>                      open the url and print its page text as Markdown
//   revb list                          tool names + one-line descriptions
//   revb skill [--full]                print the CLI guide (--full appends the live tool catalog)
//   revb endpoint                      print the published endpoint (diagnostics)

import { execSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const HERE = path.dirname(fileURLToPath(import.meta.url))
// ui_crawl / intruder_run / scan can run for minutes.
const CALL_TIMEOUT_MS = 600_000
const PAGE_CHARS = 60_000 // tool_result_more's max limit
const MAX_PAGES = 200 // 12M chars; the server keeps at most 20M across all results

// ---------------------------------------------------------------- pure helpers

/** Electron's userData for this app, computed the way src/main/index.ts does. */
export function userDataDir(env = process.env, platform = process.platform, home = os.homedir()) {
  if (env.REVER_USER_DATA_DIR) return env.REVER_USER_DATA_DIR
  // Join with the target platform's separator, not the host's.
  const join = platform === 'win32' ? path.win32.join : path.posix.join
  const appData =
    platform === 'darwin'
      ? join(home, 'Library', 'Application Support')
      : platform === 'win32'
        ? env.APPDATA || join(home, 'AppData', 'Roaming')
        : env.XDG_CONFIG_HOME || join(home, '.config')
  return join(appData, 'rever-browser')
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

/** How a result is printed: text as it is, anything else as indented JSON. */
export function format(value) {
  return (typeof value === 'string' ? value : JSON.stringify(value, null, 2)) + '\n'
}

/**
 * Turn `--key value` arguments into a tool's input, typed by the tool's schema.
 * A boolean flag is true on its own (`--full`) or takes `=false`. An array flag
 * repeats (`--hosts a --hosts b`). A value that starts with `{` or `[` is JSON.
 * An argument the schema does not list is refused with the ones it does.
 */
export function flagsToArgs(schema, argv) {
  const props = schema?.properties ?? {}
  const out = {}
  for (let i = 0; i < argv.length; i++) {
    const isFlag = argv[i].startsWith('--')
    const eq = argv[i].indexOf('=')
    const key = isFlag ? argv[i].slice(2, eq < 0 ? undefined : eq) : ''
    const prop = props[key]
    if (!prop) {
      const known = Object.keys(props).map((name) => '--' + name).join(' ')
      throw new Error(`${isFlag ? 'unknown' : 'unexpected'} argument ${argv[i]}. This tool takes: ${known || 'no arguments'}`)
    }
    let raw
    if (eq >= 0) raw = argv[i].slice(eq + 1)
    else if (prop.type === 'boolean' && !['true', 'false'].includes(argv[i + 1])) raw = 'true'
    else if (i + 1 >= argv.length) throw new Error(`--${key} needs a value`)
    else raw = argv[++i]
    const json = /^[[{]/.test(raw.trim())
    if (prop.type === 'array' && !json) {
      ;(out[key] ??= []).push(typed(raw, prop.items ?? {}, key))
    } else if (json) {
      try {
        out[key] = JSON.parse(raw)
      } catch {
        throw new Error(`--${key} takes JSON, got: ${raw}`)
      }
    } else {
      out[key] = typed(raw, prop, key)
    }
  }
  return out
}

/** One flag value as its schema type asks. */
function typed(raw, prop, key) {
  if (prop.type === 'boolean') {
    if (raw === 'true') return true
    if (raw === 'false') return false
    throw new Error(`--${key} is a boolean: write --${key} or --${key}=false`)
  }
  if (prop.type === 'integer' || prop.type === 'number') {
    const n = Number(raw)
    if (raw.trim() === '' || Number.isNaN(n) || (prop.type === 'integer' && !Number.isInteger(n))) {
      throw new Error(`--${key} is ${prop.type === 'integer' ? 'an integer' : 'a number'}, got ${raw}`)
    }
    return n
  }
  return raw
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
  process.stdout.write(format(v))
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
      tools: () => tools.map((t) => ({ name: t.name, description: t.description, inputSchema: t.inputSchema })),
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

/// The Markdown section of a `browser_snapshot` result, or null.
export function extractMarkdown(text) {
  const marker = '--- page markdown ---'
  const at = text.indexOf(marker)
  return at < 0 ? null : text.slice(at + marker.length).trim()
}

const USAGE = `revb — drive a running Rever Browser from scripts

  revb run <script.mjs | ->        run a script; every MCP tool is a global async fn
  revb call <tool> ['<json>']      call one tool and print its result
  revb call <tool> --key value     or pass its arguments as flags typed by the tool's schema
  revb describe <tool>             one tool's description and input schema as JSON
  revb md <url>                    open the url and print its page text as Markdown
  revb list                        list tools (--list works too)
  revb skill [--full]              print the scripting guide (--full: + live tool catalog)
  revb endpoint                    print the endpoint the CLI connects to`

function fail(msg) {
  process.stderr.write(`revb: ${msg}\n`)
  process.exit(1)
}

/** The live catalog entry of `name`, or a refusal naming the way out. */
async function findTool(client, name) {
  const tool = (await client.tools()).find((t) => t.name === name)
  if (!tool) fail(`no tool named ${name}. Run revb list for every tool.`)
  return tool
}

/** A call's arguments: one JSON object, or `--key value` flags typed by the tool's schema. */
async function callArgs(client, name, argv) {
  if (argv.length === 0) return {}
  if (argv[0].startsWith('--')) return flagsToArgs((await findTool(client, name)).inputSchema, argv)
  if (argv.length > 1) fail(`revb call takes one JSON object or --key value flags, got: ${argv.join(' ')}`)
  return JSON.parse(argv[0])
}

async function main(argv) {
  const [first, ...rest] = argv
  const cmd = first === '--list' ? 'list' : first
  switch (cmd) {
    case 'run':
      return cmdRun(rest[0])
    case 'call': {
      const name = rest[0]
      if (!name) fail('usage: revb call <tool> [\'<json>\'] or revb call <tool> --key value')
      return withClient(async (c) => print(await c.call(name, await callArgs(c, name, rest.slice(1)))))
    }
    case 'describe': {
      const name = rest[0]
      if (!name) fail('usage: revb describe <tool>')
      return withClient(async (c) => {
        const tool = await findTool(c, name)
        print({ name: tool.name, description: tool.description, inputSchema: tool.inputSchema })
      })
    }
    case 'md': {
      if (!rest[0]) fail('usage: revb md <url>')
      return withClient(async (c) => {
        await c.call('browser_navigate', { url: rest[0] })
        const snap = await c.call('browser_snapshot', { markdown: true })
        const text = typeof snap === 'string' ? snap : JSON.stringify(snap, null, 2)
        print(extractMarkdown(text) ?? text)
      })
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
