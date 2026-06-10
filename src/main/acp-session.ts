import { spawn, type ChildProcessByStdio } from 'node:child_process'
import { Readable, Writable } from 'node:stream'
import {
  ClientSideConnection,
  ndJsonStream,
  PROTOCOL_VERSION,
  type Agent,
  type Client,
  type RequestPermissionRequest,
  type RequestPermissionResponse,
  type SessionNotification
} from '@agentclientprotocol/sdk'
import type { WebContents } from 'electron'

import { startMcpServer } from './mcp/server'

export interface AgentDef {
  id: string
  command: string
  args: string[]
}

interface ModelInfo {
  modelId: string
  name: string
  description?: string | null
}

interface SessionEntry {
  agentDef: AgentDef
  child: ChildProcessByStdio<Writable, Readable, Readable>
  connection: ClientSideConnection
  sessionId: string
  onUpdate: ((n: SessionNotification) => void) | null
  requestPermission:
    | ((req: RequestPermissionRequest) => Promise<RequestPermissionResponse>)
    | null
  dead: boolean
  availableModels: ModelInfo[]
  currentModelId: string | null
}

const sessions = new Map<string, SessionEntry>()

function pickAutoApproveOption(req: RequestPermissionRequest): string {
  const allowAlways = req.options.find((o) => o.kind === 'allow_always')
  if (allowAlways) return allowAlways.optionId
  const allow = req.options.find((o) => o.kind.startsWith('allow'))
  return allow?.optionId ?? req.options[0]?.optionId ?? ''
}

export async function spawnAcpSession(
  agentDef: AgentDef,
  cwd: string
): Promise<{ sessionId: string }> {
  // On Windows, npm installs the agent CLI as a `.cmd` shim. Node 22's
  // CVE-2024-27980 patch refuses to spawn `.cmd`/`.bat` without `shell: true`
  // (throws EINVAL). Setting shell on Windows lets the resolved absolute
  // path execute cleanly. On POSIX the binary is a real executable / JS
  // shebang, so we leave shell off to avoid quoting surprises.
  const child = spawn(agentDef.command, agentDef.args, {
    cwd,
    stdio: ['pipe', 'pipe', 'pipe'],
    shell: process.platform === 'win32'
  }) as ChildProcessByStdio<Writable, Readable, Readable>

  child.stderr.on('data', (buf: Buffer) => {
    console.error(`[ACP ${agentDef.id}]`, buf.toString())
  })

  // PATH에 바이너리가 없거나 실행 권한이 없을 때 ENOENT/EACCES 에러가 발생한다.
  // 핸들러 없이 방치하면 main 프로세스가 죽으므로 반드시 등록한다.
  let childError: Error | null = null
  child.on('error', (e) => {
    childError = e
    console.error(`[ACP ${agentDef.id}] spawn error:`, e.message)
  })

  const input = Writable.toWeb(child.stdin) as WritableStream<Uint8Array>
  const output = Readable.toWeb(child.stdout) as ReadableStream<Uint8Array>
  const stream = ndJsonStream(input, output)

  let entryRef: SessionEntry | null = null

  const clientImpl: Client = {
    async requestPermission(
      params: RequestPermissionRequest
    ): Promise<RequestPermissionResponse> {
      // Route to the renderer's permission UI when a prompt is in flight.
      // Falls back to auto-approve if no handler is attached or the round-trip
      // fails/times out — so the agent loop can never deadlock on a missing UI.
      const handler = entryRef?.requestPermission
      if (handler) {
        try {
          return await handler(params)
        } catch (e) {
          console.error('[acp] permission round-trip failed, auto-approving:', e)
        }
      }
      return {
        outcome: { outcome: 'selected', optionId: pickAutoApproveOption(params) }
      }
    },
    async sessionUpdate(params: SessionNotification): Promise<void> {
      entryRef?.onUpdate?.(params)
    }
  }

  const connection = new ClientSideConnection((_agent: Agent) => clientImpl, stream)

  // 바이너리가 없거나 stdio가 끊겼을 때 영원히 pending되는 것을 방지하기 위해
  // initialize / newSession 모두 10초 타임아웃을 적용한다.
  function withTimeout<T>(promise: Promise<T>, ms: number, label: string): Promise<T> {
    return Promise.race([
      promise,
      new Promise<T>((_, reject) =>
        setTimeout(() => reject(new Error(`ACP ${label} timed out after ${ms}ms`)), ms)
      )
    ])
  }

  await withTimeout(
    connection.initialize({ protocolVersion: PROTOCOL_VERSION, clientCapabilities: {} }),
    10_000,
    'initialize'
  ).catch((e) => {
    // childError가 있으면 더 구체적인 메시지를 포함해 던진다.
    if (childError) throw new Error(`Failed to spawn agent "${agentDef.command}": ${childError.message}`)
    throw e
  })

  const mcp = await startMcpServer()
  const result = await withTimeout(
    connection.newSession({
      cwd,
      mcpServers: [
        {
          type: 'http',
          name: 'rever-traffic',
          url: mcp.url,
          headers: []
        }
      ]
    }),
    10_000,
    'newSession'
  )

  console.log('[acp:newSession] result keys:', Object.keys(result), 'models:', JSON.stringify((result as { models?: unknown }).models))
  const modelState = (result as { models?: { availableModels?: ModelInfo[]; currentModelId?: string } | null }).models
  const entry: SessionEntry = {
    agentDef,
    child,
    connection,
    sessionId: result.sessionId,
    onUpdate: null,
    requestPermission: null,
    dead: false,
    availableModels: modelState?.availableModels ?? [],
    currentModelId: modelState?.currentModelId ?? null
  }
  entryRef = entry
  sessions.set(result.sessionId, entry)

  // child가 닫히면 세션을 dead로 표시한다. 진행 중인 prompt는 connection
  // 레벨에서 끊기므로 promptAcpSession 내의 connection.prompt()가 자연스럽게
  // reject된다 (ndJsonStream이 closed stream에서 에러를 던진다).
  child.on('close', (code) => {
    entry.dead = true
    sessions.delete(result.sessionId)
    console.warn(`[ACP ${agentDef.id}] child closed with code ${code}`)
  })

  return { sessionId: result.sessionId }
}

export async function promptAcpSession(
  sessionId: string,
  text: string,
  onUpdate: (n: SessionNotification) => void,
  requestPermission?: (req: RequestPermissionRequest) => Promise<RequestPermissionResponse>
): Promise<{ stopReason: string }> {
  const entry = sessions.get(sessionId)
  if (!entry) throw new Error(`unknown ACP session: ${sessionId}`)
  if (entry.dead) throw new Error(`ACP session is dead: ${sessionId}`)

  entry.onUpdate = onUpdate
  entry.requestPermission = requestPermission ?? null
  try {
    const res = await entry.connection.prompt({
      sessionId: entry.sessionId,
      prompt: [{ type: 'text', text }]
    })
    return { stopReason: res.stopReason }
  } finally {
    entry.onUpdate = null
    entry.requestPermission = null
  }
}

export async function cancelAcpSession(sessionId: string): Promise<void> {
  const entry = sessions.get(sessionId)
  if (!entry || entry.dead) return
  await entry.connection.cancel({ sessionId: entry.sessionId }).catch(() => null)
}

export async function killAcpSession(sessionId: string): Promise<void> {
  const entry = sessions.get(sessionId)
  if (!entry) return
  entry.dead = true
  sessions.delete(sessionId)
  entry.child.kill()
}

export function getSessionModelState(
  sessionId: string
): { availableModels: ModelInfo[]; currentModelId: string | null } | null {
  const entry = sessions.get(sessionId)
  if (!entry) return null
  return {
    availableModels: entry.availableModels,
    currentModelId: entry.currentModelId
  }
}

export async function setSessionModel(sessionId: string, modelId: string): Promise<void> {
  const entry = sessions.get(sessionId)
  if (!entry) throw new Error(`unknown ACP session: ${sessionId}`)
  if (entry.dead) throw new Error(`ACP session is dead: ${sessionId}`)
  const conn = entry.connection as unknown as {
    unstable_setSessionModel?: (params: { sessionId: string; modelId: string }) => Promise<unknown>
  }
  if (!conn.unstable_setSessionModel) {
    throw new Error('ACP SDK does not expose unstable_setSessionModel on this connection')
  }
  await conn.unstable_setSessionModel({ sessionId: entry.sessionId, modelId })
  entry.currentModelId = modelId
}
