import { spawn, type ChildProcessByStdio } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import type { Readable, Writable } from 'node:stream'

import type {
  RequestPermissionRequest,
  RequestPermissionResponse,
  SessionNotification
} from '@agentclientprotocol/sdk'

import { agentEnv } from '../agent-env'
import { startMcpServer } from '../mcp/server'
import { shellSafeSpawn } from '../win-shell'
import { fallbackOption } from '../../shared/risky-tools'
import { startTurnPulse } from './turn-pulse'
import {
  claudeCliArgs,
  controlRequest,
  errorResponse,
  flagSettingsRequest,
  modelState,
  optionsFromSettings,
  newTranslateState,
  permissionResponse,
  toPermissionRequest,
  translate,
  turnOutcome,
  userMessage,
  type CliModel,
  type CliOptions
} from './claude-cli-protocol'

// Drives the user's own `claude` CLI headless over stream-json — no ACP
// adapter in between. Auth is whatever the CLI is signed in with (Pro/Max
// subscription or API key); this process never sees a credential.

const PREFIX = 'claude-cli:'
const CONTROL_TIMEOUT_MS = 25_000

type Child = ChildProcessByStdio<Writable, Readable, Readable>
type Permission = (req: RequestPermissionRequest) => Promise<RequestPermissionResponse>

interface Turn {
  resolve: (r: { stopReason: string }) => void
  reject: (e: Error) => void
  interrupted: boolean
}

interface Entry {
  child: Child
  dead: boolean
  stderrTail: string
  pending: Map<string, { resolve: (r: Record<string, unknown>) => void; reject: (e: Error) => void }>
  turn: Turn | null
  /** Settles when the current turn's result arrives (or the process dies). */
  turnDone: Promise<void>
  onUpdate: ((n: SessionNotification) => void) | null
  requestPermission: Permission | null
  models: CliModel[]
  currentModel: string | null
  options: CliOptions
  state: ReturnType<typeof newTranslateState>
}

const sessions = new Map<string, Entry>()
let reqSeq = 0

export const isClaudeCliSession = (sessionId: string): boolean => sessionId.startsWith(PREFIX)

function control(entry: Entry, request: Record<string, unknown>): Promise<Record<string, unknown>> {
  const id = `rev_${++reqSeq}`
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      entry.pending.delete(id)
      reject(new Error(`claude ${String(request.subtype)} timed out`))
    }, CONTROL_TIMEOUT_MS)
    entry.pending.set(id, {
      resolve: (r) => (clearTimeout(timer), resolve(r)),
      reject: (e) => (clearTimeout(timer), reject(e))
    })
    entry.child.stdin.write(controlRequest(id, request))
  })
}

async function onPermission(entry: Entry, sessionId: string, requestId: string, req: Record<string, unknown>): Promise<void> {
  const acpReq = toPermissionRequest(sessionId, req)
  let answer: RequestPermissionResponse
  try {
    if (!entry.requestPermission) throw new Error('no prompt in flight')
    answer = await entry.requestPermission(acpReq)
  } catch (e) {
    // Same fallback the ACP path used: auto-allow unless the tool is risky.
    console.error('[claude-cli] permission round-trip failed, using fallback:', e)
    answer = { outcome: { outcome: 'selected', optionId: fallbackOption(acpReq) } }
  }
  if (!entry.dead) entry.child.stdin.write(permissionResponse(requestId, answer, req.input))
}

function onMessage(entry: Entry, sessionId: string, msg: Record<string, any>): void {
  if (msg.type === 'control_response') {
    const r = msg.response ?? {}
    const p = entry.pending.get(r.request_id)
    if (!p) return
    entry.pending.delete(r.request_id)
    if (r.subtype === 'error') p.reject(new Error(String(r.error)))
    else p.resolve(r.response ?? {})
    return
  }
  if (msg.type === 'control_request') {
    if (msg.request?.subtype === 'can_use_tool') void onPermission(entry, sessionId, msg.request_id, msg.request)
    else entry.child.stdin.write(errorResponse(msg.request_id, `unsupported control request: ${msg.request?.subtype}`))
    return
  }
  if (msg.type === 'result') {
    const turn = entry.turn
    entry.turn = null
    if (!turn) return
    const out = turnOutcome(msg, turn.interrupted)
    if ('error' in out) turn.reject(new Error(out.error))
    else turn.resolve(out)
    return
  }
  for (const n of translate(msg, sessionId, entry.state)) entry.onUpdate?.(n)
}

export async function spawnClaudeCliSession(command: string, cwd: string): Promise<{ sessionId: string }> {
  const mcp = await startMcpServer()
  // shellSafeSpawn quotes for cmd.exe on Windows: the empty --setting-sources
  // value and the --mcp-config JSON are otherwise dropped/mangled there.
  const launch = shellSafeSpawn(command, claudeCliArgs({ mcpUrl: mcp.url, authHeader: mcp.authHeader }))
  const child = spawn(launch.command, launch.args, {
    cwd,
    stdio: ['pipe', 'pipe', 'pipe'],
    env: agentEnv(command),
    shell: launch.shell
  }) as Child
  const sessionId = PREFIX + randomUUID()
  const entry: Entry = {
    child,
    dead: false,
    stderrTail: '',
    pending: new Map(),
    turn: null,
    turnDone: Promise.resolve(),
    onUpdate: null,
    requestPermission: null,
    models: [],
    currentModel: null,
    options: { effort: null, fastMode: false },
    state: newTranslateState()
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
        onMessage(entry, sessionId, JSON.parse(raw))
      } catch (e) {
        console.error('[claude-cli] bad line:', raw.slice(0, 200), e)
      }
    }
  })
  child.stderr.on('data', (b: Buffer) => {
    entry.stderrTail = (entry.stderrTail + b.toString()).slice(-2000)
  })
  const die = (why: string): void => {
    if (entry.dead) return
    entry.dead = true
    sessions.delete(sessionId)
    const err = new Error(`${why}${entry.stderrTail ? `: ${entry.stderrTail.trim().slice(-500)}` : ''}`)
    for (const p of entry.pending.values()) p.reject(err)
    entry.pending.clear()
    entry.turn?.reject(err)
    entry.turn = null
  }
  child.on('error', (e) => die(`failed to start ${command}: ${e.message}`))
  child.on('close', (code) => die(`claude exited with code ${code}`))

  // initialize doubles as the readiness check and returns the model list.
  const init = await control(entry, { subtype: 'initialize' })
  entry.models = Array.isArray(init.models) ? (init.models as CliModel[]) : []
  entry.currentModel = entry.models.some((m) => m.value === 'default') ? 'default' : (entry.models[0]?.value ?? null)
  await refreshOptions(entry)
  sessions.set(sessionId, entry)
  return { sessionId }
}

// Effort depends on the model (a switch can reset it), so re-read what the CLI applied.
async function refreshOptions(entry: Entry): Promise<void> {
  try {
    entry.options = optionsFromSettings(await control(entry, { subtype: 'get_settings' }))
  } catch (e) {
    console.warn('[claude-cli] get_settings failed:', e)
  }
}

function live(sessionId: string): Entry {
  const entry = sessions.get(sessionId)
  if (!entry || entry.dead) throw new Error(`Claude Code session is not running: ${sessionId}`)
  return entry
}

export async function promptClaudeCliSession(
  sessionId: string,
  text: string,
  onUpdate: (n: SessionNotification) => void,
  requestPermission?: Permission
): Promise<{ stopReason: string }> {
  const entry = live(sessionId)
  if (entry.turn) {
    // The old turn's `result` must land before a new turn starts, or it would resolve the new one.
    const prev = entry.turnDone
    await cancelClaudeCliSession(sessionId)
    await Promise.race([prev, new Promise((r) => setTimeout(r, 6_000))])
  }

  entry.onUpdate = onUpdate
  entry.requestPermission = requestPermission ?? null
  const stopPulse = startTurnPulse((n) => {
    if (!entry.dead) onUpdate(n)
  }, sessionId)
  try {
    const turn = new Promise<{ stopReason: string }>((resolve, reject) => {
      entry.turn = { resolve, reject, interrupted: false }
      entry.child.stdin.write(userMessage(text))
    })
    entry.turnDone = turn.then(
      () => undefined,
      () => undefined
    )
    return await turn
  } finally {
    stopPulse()
    // A newer prompt may already own these.
    if (entry.onUpdate === onUpdate) {
      entry.onUpdate = null
      entry.requestPermission = null
    }
  }
}

export async function cancelClaudeCliSession(sessionId: string): Promise<void> {
  const entry = sessions.get(sessionId)
  if (!entry || entry.dead || !entry.turn) return
  entry.turn.interrupted = true
  await control(entry, { subtype: 'interrupt' }).catch((e) => console.error('[claude-cli] interrupt failed:', e))
}

export async function killClaudeCliSession(sessionId: string): Promise<void> {
  const entry = sessions.get(sessionId)
  if (!entry) return
  entry.child.kill()
}

export function getClaudeCliModelState(sessionId: string): ReturnType<typeof modelState> | null {
  const entry = sessions.get(sessionId)
  return entry ? modelState(entry.models, entry.currentModel, entry.options) : null
}

export async function setClaudeCliModel(sessionId: string, modelId: string): Promise<void> {
  const entry = live(sessionId)
  await control(entry, { subtype: 'set_model', model: modelId })
  entry.currentModel = modelId
  await refreshOptions(entry)
}

/** Reasoning effort / fast mode, applied to the running session (no restart). */
export async function setClaudeCliOption(sessionId: string, key: 'effort' | 'fastMode', value: string | boolean): Promise<void> {
  const entry = live(sessionId)
  await control(entry, flagSettingsRequest(key, value))
  await refreshOptions(entry)
}
