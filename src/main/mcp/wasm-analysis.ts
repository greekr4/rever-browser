import { spawn } from 'node:child_process'
import { writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { randomUUID } from 'node:crypto'
import { createRequire } from 'node:module'

import { getRequest, listRequests, type StoredRequest } from '../traffic-store'
import { listScripts, grepBody } from './script-analysis'
import { ok, err, errorMessage } from './utils'

export const WABT_INSTALL_HINT = 'wabt not installed. Install with: bun add -d wabt'

// ── refetch body encoding (chrome-cdp.ts fallback path) ──────────────────────

// Binary MIME types whose bytes are mangled by a utf8 decode. `.wasm` is the
// reason this exists: a cache- or service-worker-delivered module re-fetched by
// refetchBody() must keep its `\0asm` magic and every non-utf8 byte intact.
const BINARY_MIME_PREFIXES = [
  'application/wasm',
  'application/octet-stream',
  'image/',
  'video/',
  'audio/',
  'font/'
]

/** The charset from a Content-Type header (`text/html; charset=euc-kr`). */
export function charsetFromContentType(contentType: string | undefined): string | undefined {
  const m = /charset\s*=\s*["']?([^;"'\s]+)/i.exec(contentType ?? '')
  return m ? m[1].toLowerCase() : undefined
}

/**
 * Decode bytes with the charset the response declared. Korean sites still
 * serve euc-kr/cp949, and decoding those as utf8 yields mojibake — the whole
 * body reads as `\uFFFD`. TextDecoder covers every label Chrome does (Electron
 * ships full ICU); an unknown label throws, so fall back to utf8.
 */
function decodeText(buf: Buffer, charset: string | undefined): string {
  let text: string
  try {
    text = new TextDecoder(charset ?? 'utf-8', { fatal: false }).decode(buf)
  } catch {
    text = buf.toString('utf8')
  }
  // A utf8 BOM survives decoding as a zero-width U+FEFF that breaks JSON.parse
  // and shows up as a stray glyph in the body panes.
  return text.charCodeAt(0) === 0xfeff ? text.slice(1) : text
}

/**
 * Decide how to store a re-fetched response body. Binary MIME types are
 * base64-encoded (lossless); everything else is decoded as text using the
 * declared charset. Pure and electron-free so it is unit-testable under vitest.
 */
export function encodeRefetchedBody(
  buf: Buffer,
  mime: string | undefined,
  charset?: string
): { responseBody: string; responseBodyBase64: boolean } {
  const isBinary = !!mime && BINARY_MIME_PREFIXES.some((p) => mime.startsWith(p))
  return isBinary
    ? { responseBody: buf.toString('base64'), responseBodyBase64: true }
    : { responseBody: decodeText(buf, charset), responseBodyBase64: false }
}

// ── captured-body access + listing ───────────────────────────────────────────

export function isWasm(r: { mimeType?: string; url: string }): boolean {
  return r.mimeType === 'application/wasm' || /\.wasm(\?|$)/i.test(r.url)
}

/** Captured WASM modules (application/wasm body or .wasm URL), largest first. */
export function listWasm(host?: string): StoredRequest[] {
  return listRequests({ host, limit: 200 })
    .filter(isWasm)
    .sort((a, b) => bodyByteLength(b) - bodyByteLength(a))
}

function bodyByteLength(r: StoredRequest): number {
  if (!r.responseBody) return 0
  return r.responseBodyBase64
    ? Math.floor(r.responseBody.length * 0.75)
    : r.responseBody.length
}

/** Decode a captured WASM body to a Buffer, or return an error string. */
export function getWasmBuffer(requestId: string): Buffer | string {
  const e = getRequest(requestId)
  if (!e) return `unknown requestId: ${requestId}`
  if (!e.responseBody)
    return 'that request has no captured body (open the page so it loads, then try list_wasm)'
  return e.responseBodyBase64
    ? Buffer.from(e.responseBody, 'base64')
    : Buffer.from(e.responseBody, 'binary')
}

// ── wabt disassembly ─────────────────────────────────────────────────────────

type WabtImporter = () => Promise<unknown>
const defaultImportWabt: WabtImporter = () => import('wabt')

/**
 * WAT baseline — in-process wabt JS API. Always available once `wabt` is
 * installed; no subprocess, no temp file. Throws WABT_INSTALL_HINT if the
 * module cannot be loaded.
 */
export async function wasmToWat(
  wasm: Buffer,
  importWabt: WabtImporter = defaultImportWabt
): Promise<string> {
  let init: () => Promise<WabtRuntime>
  try {
    const mod = (await importWabt()) as { default?: unknown }
    init = ((mod && mod.default) || mod) as () => Promise<WabtRuntime>
  } catch {
    throw new Error(WABT_INSTALL_HINT)
  }
  const wabt = await init()
  const module = wabt.readWasm(new Uint8Array(wasm), { readDebugNames: true })
  try {
    return module.toText({})
  } finally {
    module.destroy()
  }
}

interface WabtRuntime {
  readWasm(buf: Uint8Array, opts: { readDebugNames?: boolean }): WabtWasmModule
}
interface WabtWasmModule {
  toText(opts: Record<string, unknown>): string
  destroy(): void
}

const WASM_TIMEOUT_MS = 30_000
const WASM_MAX_OUTPUT = 5 * 1024 * 1024

/**
 * Shell out to a bundled `bin/<tool>` Emscripten-node script (wasm-decompile,
 * wasm2c, wasm-objdump …) via Electron-as-node — these produce output the
 * in-process JS API can't. Writes a temp file (wabt reads a path, not stdin),
 * passes `extraArgs` before it, and always cleans up. Callers degrade on error.
 */
export function runWabtBin(tool: string, wasm: Buffer, extraArgs: string[] = []): Promise<string> {
  return new Promise((resolve, reject) => {
    let binPath: string
    try {
      const require = createRequire(import.meta.url)
      const binDir = path.join(path.dirname(require.resolve('wabt/package.json')), 'bin')
      binPath = path.join(binDir, tool)
    } catch {
      reject(new Error(WABT_INSTALL_HINT))
      return
    }

    const tmp = path.join(tmpdir(), `rever-${randomUUID()}.wasm`)
    try {
      writeFileSync(tmp, wasm)
    } catch (e) {
      reject(e)
      return
    }
    const cleanup = (): void => rmSync(tmp, { force: true })

    let proc: ReturnType<typeof spawn>
    try {
      proc = spawn(process.execPath, [binPath, ...extraArgs, tmp], {
        stdio: ['ignore', 'pipe', 'pipe'],
        env: { ...process.env, ELECTRON_RUN_AS_NODE: '1' }
      })
    } catch (e) {
      cleanup()
      reject(e)
      return
    }

    const chunks: Buffer[] = []
    let total = 0
    let killed = false
    const timer = setTimeout(() => {
      killed = true
      proc.kill('SIGKILL')
    }, WASM_TIMEOUT_MS)

    proc.on('error', (e: NodeJS.ErrnoException) => {
      clearTimeout(timer)
      cleanup()
      reject(e.code === 'ENOENT' ? new Error(WABT_INSTALL_HINT) : e)
    })
    proc.stdout!.on('data', (c: Buffer) => {
      total += c.length
      if (total > WASM_MAX_OUTPUT) {
        killed = true
        proc.kill('SIGKILL')
        return
      }
      chunks.push(c)
    })
    proc.stderr!.on('data', () => {})
    proc.on('close', (code) => {
      clearTimeout(timer)
      cleanup()
      if (killed) {
        reject(new Error(`${tool} timed out or output exceeded ${WASM_MAX_OUTPUT} bytes`))
        return
      }
      if (code !== 0 && chunks.length === 0) {
        reject(new Error(`${tool} exited with code ${code}`))
        return
      }
      resolve(Buffer.concat(chunks).toString('utf8'))
    })
  })
}

/** C-like decompilation (`wasm-decompile`). */
export const wasmDecompileFull = (wasm: Buffer): Promise<string> =>
  runWabtBin('wasm-decompile', wasm)

/** Full C source (`wasm2c`) — the most readable output for complex routines. */
export const wasmToC = (wasm: Buffer): Promise<string> => runWabtBin('wasm2c', wasm)

/** Module summary + symbol names (`wasm-objdump -x`): types, imports, exports. */
export const wasmObjdump = (wasm: Buffer): Promise<string> =>
  runWabtBin('wasm-objdump', wasm, ['-x'])

export type WasmFormat = 'wat' | 'decompile' | 'c'

/**
 * Render flow behind the tool. Returns an MCP `ok`/`err` content object (kept
 * out of the tool closure so it is directly unit-testable). Non-WAT formats
 * shell out and degrade to WAT if that path is unavailable; WAT (in-process)
 * is the guaranteed baseline.
 */
export async function decompileRequest(
  requestId: string,
  format: WasmFormat,
  importWabt: WabtImporter = defaultImportWabt
): Promise<ReturnType<typeof ok> | ReturnType<typeof err>> {
  const bytes = getWasmBuffer(requestId)
  if (typeof bytes === 'string') return err(bytes)
  if (format === 'decompile' || format === 'c') {
    try {
      return ok(await (format === 'c' ? wasmToC(bytes) : wasmDecompileFull(bytes)))
    } catch {
      // degrade gracefully to WAT-only
    }
  }
  try {
    return ok(await wasmToWat(bytes, importWabt))
  } catch (e) {
    return err(errorMessage(e))
  }
}

// ── strings / constants extraction ───────────────────────────────────────────

/**
 * `strings`-style scan of a WASM body: printable-ASCII runs (data section
 * literals, symbol names, algorithm ids, keys, URLs). Pure — no wabt needed.
 * Deduped, in first-seen order, optionally filtered by a regex source string.
 */
export function extractWasmStrings(
  buf: Buffer,
  opts: { min?: number; pattern?: string } = {}
): string[] {
  const min = opts.min ?? 4
  const re = opts.pattern ? new RegExp(opts.pattern) : null
  const seen = new Set<string>()
  const out: string[] = []
  let cur = ''
  const flush = (): void => {
    if (cur.length >= min && (!re || re.test(cur)) && !seen.has(cur)) {
      seen.add(cur)
      out.push(cur)
    }
    cur = ''
  }
  for (const byte of buf) {
    if (byte >= 0x20 && byte <= 0x7e) cur += String.fromCharCode(byte)
    else flush()
  }
  flush()
  return out
}

// ── export ↔ JS call-site cross-reference ────────────────────────────────────

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

/** Export names declared in a WAT dump, e.g. `(export "checksum" ...)`. */
export function parseWatExports(wat: string): string[] {
  const out: string[] = []
  const re = /\(export\s+"([^"]+)"/g
  let m: RegExpExecArray | null
  while ((m = re.exec(wat)) !== null) out.push(m[1])
  return out
}

export interface XrefHit {
  requestId: string
  url: string
  snippet: string
}
export interface ExportXref {
  export: string
  referenced: boolean
  hits: XrefHit[]
}

/**
 * Bridge WASM ↔ JS: for each exported function, grep the captured script
 * bodies for its name so the agent sees where (and whether) the module is
 * called — e.g. an export invoked right before a request is signed.
 */
export async function xrefExports(
  requestId: string,
  importWabt: WabtImporter = defaultImportWabt
): Promise<ExportXref[] | string> {
  const bytes = getWasmBuffer(requestId)
  if (typeof bytes === 'string') return bytes
  const wat = await wasmToWat(bytes, importWabt)
  const exports = parseWatExports(wat)
  const scripts = listScripts({ limit: 200 })
  return exports.map((name) => {
    const re = new RegExp('\\b' + escapeRegExp(name) + '\\b', 'g')
    const hits: XrefHit[] = []
    for (const s of scripts) {
      if (!s.responseBody) continue
      for (const h of grepBody(s.responseBody, re, { max: 5, before: 40, after: 40 })) {
        hits.push({ requestId: s.requestId, url: s.url, snippet: h.snippet })
      }
    }
    return { export: name, referenced: hits.length > 0, hits }
  })
}
