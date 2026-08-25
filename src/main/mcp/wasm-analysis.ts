import { spawn } from 'node:child_process'
import { writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { randomUUID } from 'node:crypto'
import { createRequire } from 'node:module'

import { getRequest, listRequests, type StoredRequest } from '../traffic-store'
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

/**
 * Decide how to store a re-fetched response body. Binary MIME types are
 * base64-encoded (lossless); everything else stays utf8 text. Pure and
 * electron-free so it is unit-testable under vitest.
 */
export function encodeRefetchedBody(
  buf: Buffer,
  mime: string | undefined
): { responseBody: string; responseBodyBase64: boolean } {
  const isBinary = !!mime && BINARY_MIME_PREFIXES.some((p) => mime.startsWith(p))
  return isBinary
    ? { responseBody: buf.toString('base64'), responseBodyBase64: true }
    : { responseBody: buf.toString('utf8'), responseBodyBase64: false }
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
 * C-like decompilation — the in-process API can't produce it, so shell out to
 * the bundled `bin/wasm-decompile` Emscripten-node script via Electron-as-node
 * (needs no external node on PATH). Writes a temp file (wabt reads a path, not
 * stdin) and always cleans it up. The caller degrades to WAT on any failure.
 */
export function wasmDecompileFull(wasm: Buffer): Promise<string> {
  return new Promise((resolve, reject) => {
    let binPath: string
    try {
      const require = createRequire(import.meta.url)
      const binDir = path.join(path.dirname(require.resolve('wabt/package.json')), 'bin')
      binPath = path.join(binDir, 'wasm-decompile')
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
      proc = spawn(process.execPath, [binPath, tmp], {
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
        reject(new Error(`wasm-decompile timed out or output exceeded ${WASM_MAX_OUTPUT} bytes`))
        return
      }
      if (code !== 0 && chunks.length === 0) {
        reject(new Error(`wasm-decompile exited with code ${code}`))
        return
      }
      resolve(Buffer.concat(chunks).toString('utf8'))
    })
  })
}

/**
 * Full decompile flow behind the tool. Returns an MCP `ok`/`err` content object
 * (kept out of the tool closure so it is directly unit-testable). `full:true`
 * attempts wasm-decompile and degrades to WAT if that path is unavailable.
 */
export async function decompileRequest(
  requestId: string,
  full: boolean,
  importWabt: WabtImporter = defaultImportWabt
): Promise<ReturnType<typeof ok> | ReturnType<typeof err>> {
  const bytes = getWasmBuffer(requestId)
  if (typeof bytes === 'string') return err(bytes)
  if (full) {
    try {
      return ok(await wasmDecompileFull(bytes))
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
