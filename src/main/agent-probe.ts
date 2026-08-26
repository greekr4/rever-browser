import { spawn, type ChildProcessByStdio } from 'node:child_process'
import { Readable, Writable } from 'node:stream'
import Anthropic from '@anthropic-ai/sdk'
import OpenAI from 'openai'
import {
  ClientSideConnection,
  ndJsonStream,
  PROTOCOL_VERSION,
  type Agent,
  type Client
} from '@agentclientprotocol/sdk'

import { agentEnv } from './acp-session'
import { detectAgents } from './acp-detect'
import { detectAgentLogins, type LoginAgentId, type LoginInfo } from './agent-login'
import { getApiKey } from './settings'

// Verifies an agent actually works, rather than only checking that a binary
// exists on PATH. ACP agents get a real `initialize` handshake (catches a
// broken install without burning tokens); API providers get a cheap
// authenticated GET (catches an expired or mistyped key).

export type AgentStatus =
  | 'ready'
  | 'not-installed'
  | 'needs-key'
  | 'needs-login'
  | 'auth-failed'
  | 'failed'

export interface AgentProbeDef {
  id: string
  provider?: 'acp' | 'anthropic' | 'openai'
  command: string
  fallbackBins?: string[]
}

export interface AgentHealth {
  id: string
  status: AgentStatus
  /** Absolute path of the ACP binary, or null for API providers. */
  resolvedPath: string | null
  /** Short renderable reason. Never contains a token or key. */
  detail: string | null
  /** True when an existing CLI login covers this agent — no setup needed. */
  auto: boolean
  /** Plan the existing login advertises ('max', 'chatgpt'), or null. */
  plan: string | null
}

const HANDSHAKE_TIMEOUT_MS = 10_000
const API_TIMEOUT_MS = 10_000

function isAuthError(e: unknown): boolean {
  const status = (e as { status?: number })?.status
  return status === 401 || status === 403
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/**
 * Spawn the ACP binary and complete only the `initialize` handshake, then kill
 * it. Proves the binary runs and speaks ACP without creating a session.
 */
async function handshake(command: string): Promise<{ ok: true } | { ok: false; detail: string }> {
  let child: ChildProcessByStdio<Writable, Readable, Readable> | null = null
  try {
    child = spawn(command, [], {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: agentEnv(command),
      shell: process.platform === 'win32'
    }) as ChildProcessByStdio<Writable, Readable, Readable>
  } catch (e) {
    return { ok: false, detail: message(e) }
  }

  let spawnError: string | null = null
  child.on('error', (e) => {
    spawnError = e.message
  })
  // Drain stderr so a chatty agent can't fill the pipe buffer and stall.
  child.stderr.resume()

  const noopClient: Client = {
    async requestPermission() {
      throw new Error('not expected during probe')
    },
    async sessionUpdate() {}
  }

  try {
    const stream = ndJsonStream(
      Writable.toWeb(child.stdin) as WritableStream<Uint8Array>,
      Readable.toWeb(child.stdout) as ReadableStream<Uint8Array>
    )
    const connection = new ClientSideConnection((_agent: Agent) => noopClient, stream)
    await Promise.race([
      connection.initialize({ protocolVersion: PROTOCOL_VERSION, clientCapabilities: {} }),
      new Promise<never>((_, reject) =>
        setTimeout(
          () => reject(new Error(`no ACP response in ${HANDSHAKE_TIMEOUT_MS / 1000}s`)),
          HANDSHAKE_TIMEOUT_MS
        )
      )
    ])
    return { ok: true }
  } catch (e) {
    return { ok: false, detail: spawnError ?? message(e) }
  } finally {
    child.kill('SIGKILL')
  }
}

async function probeAnthropic(): Promise<AgentHealth> {
  const base = { id: 'anthropic', resolvedPath: null, auto: false, plan: null }
  const apiKey = getApiKey('anthropic')
  if (!apiKey) return { ...base, status: 'needs-key', detail: null }
  try {
    await new Anthropic({ apiKey, timeout: API_TIMEOUT_MS }).models.list({ limit: 1 })
    return { ...base, status: 'ready', detail: null, plan: 'api-key' }
  } catch (e) {
    if (isAuthError(e)) return { ...base, status: 'auth-failed', detail: 'Key rejected' }
    return { ...base, status: 'failed', detail: message(e) }
  }
}

async function probeOpenAi(): Promise<AgentHealth> {
  const base = { id: 'openai', resolvedPath: null, auto: false, plan: null }
  const apiKey = getApiKey('openai')
  if (!apiKey) return { ...base, status: 'needs-key', detail: null }
  try {
    await new OpenAI({ apiKey, timeout: API_TIMEOUT_MS }).models.list()
    return { ...base, status: 'ready', detail: null, plan: 'api-key' }
  } catch (e) {
    if (isAuthError(e)) return { ...base, status: 'auth-failed', detail: 'Key rejected' }
    return { ...base, status: 'failed', detail: message(e) }
  }
}

async function probeAcp(
  def: AgentProbeDef,
  resolvedPath: string | null,
  login: LoginInfo
): Promise<AgentHealth> {
  const base = { id: def.id, resolvedPath, auto: false, plan: login.plan }
  if (!resolvedPath) return { ...base, status: 'not-installed', detail: null, plan: null }

  const result = await handshake(resolvedPath)
  if (!result.ok) return { ...base, status: 'failed', detail: result.detail }
  // The binary works; a missing CLI login is the remaining blocker.
  if (!login.loggedIn) return { ...base, status: 'needs-login', detail: null }
  return { ...base, status: 'ready', detail: null, auto: true }
}

/**
 * Probe every agent in `defs` concurrently. PATH detection and login detection
 * each run once and are shared across the entries.
 */
export async function probeAgents(defs: AgentProbeDef[]): Promise<AgentHealth[]> {
  const acpDefs = defs.filter((d) => (d.provider ?? 'acp') === 'acp' && d.command)
  const [detected, logins] = await Promise.all([
    detectAgents(acpDefs.map((d) => ({ command: d.command, fallbackBins: d.fallbackBins }))),
    detectAgentLogins()
  ])
  const paths = new Map(detected.map((r) => [r.command, r.resolvedPath]))
  const noLogin: LoginInfo = { loggedIn: false, plan: null }

  return Promise.all(
    defs.map((def) => {
      if (def.provider === 'anthropic') return probeAnthropic()
      if (def.provider === 'openai') return probeOpenAi()
      const login = logins[def.id as LoginAgentId] ?? noLogin
      return probeAcp(def, paths.get(def.command) ?? null, login)
    })
  )
}
