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
import { modelStateFromSession, type ModelInfo } from './acp-models'
import { isStaleEmptyTurn } from './acp-turn'
import { fallbackOption } from '../shared/risky-tools'
import { startTurnPulse } from './providers/turn-pulse'

// agentEnv는 electron을 거의 끌어오지 않는 순수 로직이라 별도 모듈로 분리했다
// (vitest 직접 임포트용). 기존 임포트 경로(acp-session) 호환을 위해 재수출.
import { agentEnv } from './agent-env'
import { shellSafeSpawn } from './win-shell'
export { agentEnv }

export interface AgentDef {
  id: string
  command: string
  args: string[]
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
  modelConfigId: string | null
  // Settle-tracking promise for the in-flight prompt (null when idle). Used to
  // avoid issuing concurrent prompts on one ACP session after a Stop.
  activePrompt: Promise<void> | null
  // True when the last prompt rejected (API error etc.). claude-agent-acp then
  // leaves a stale "idle" message queued, so the next prompt returns an empty
  // end_turn instantly — promptAcpSession re-issues it once in that case.
  lastPromptFailed: boolean
}

const sessions = new Map<string, SessionEntry>()

// Updates that arrive with no prompt in flight: Claude Code runs a turn on its
// own when a background task finishes, and newer claude-agent-acp streams it
// between prompts. Without a sink they would be dropped.
let idleUpdateSink: ((sessionId: string, n: SessionNotification) => void) | null = null

export function onIdleAcpUpdate(sink: (sessionId: string, n: SessionNotification) => void): void {
  idleUpdateSink = sink
}

// Sent instead of the user's text when re-driving a stale empty turn (see
// SessionEntry.lastPromptFailed). The pending user messages are still in the
// agent's input queue; this just makes the model process them.
const STALE_TURN_NUDGE = '(continue — answer the previous message)'

export async function spawnAcpSession(
  agentDef: AgentDef,
  cwd: string
): Promise<{ sessionId: string }> {
  // On Windows, npm installs the agent CLI as a `.cmd` shim. Node 22's
  // CVE-2024-27980 patch refuses to spawn `.cmd`/`.bat` without `shell: true`
  // (throws EINVAL). Setting shell on Windows lets the resolved absolute
  // path execute cleanly. On POSIX the binary is a real executable / JS
  // shebang, so we leave shell off to avoid quoting surprises.
  // shellSafeSpawn quotes each arg for cmd.exe so they survive the shell.
  const launch = shellSafeSpawn(agentDef.command, agentDef.args)
  const child = spawn(launch.command, launch.args, {
    cwd,
    stdio: ['pipe', 'pipe', 'pipe'],
    env: agentEnv(agentDef.command),
    shell: launch.shell
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
      // Risky tools are rejected instead: they must never run unseen.
      const handler = entryRef?.requestPermission
      if (handler) {
        try {
          return await handler(params)
        } catch (e) {
          console.error('[acp] permission round-trip failed, auto-approving:', e)
        }
      }
      return {
        outcome: { outcome: 'selected', optionId: fallbackOption(params) }
      }
    },
    async sessionUpdate(params: SessionNotification): Promise<void> {
      if (!entryRef || entryRef.dead) return
      if (entryRef.onUpdate) entryRef.onUpdate(params)
      else idleUpdateSink?.(entryRef.sessionId, params)
    },
    // Agent-specific extensions (claude-agent-acp sends `_auth/status_update`).
    // Nothing here uses them; without a handler each one logs "Method not found".
    async extNotification(): Promise<void> {}
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
    25_000,
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
          headers: [
            { name: 'Authorization', value: mcp.authHeader },
            { name: 'X-Rever-Catalog', value: 'follow' }
          ]
        }
      ],
      // settingSources를 빈 배열로 두지 않으면 claude-agent-acp가 사용자의
      // ~/.claude.json에 등록된 모든 개인 MCP 서버(playwright/notion/postgres 등
      // 20여 개)를 매 세션마다 띄운다. 이 부팅이 ~9.5초 걸려 아래 newSession
      // 10초 타임아웃을 자주 넘긴다("ACP newSession timed out"). 에이전트에는
      // 위에서 명시한 rever-traffic MCP만 있으면 되므로 개인 설정 로딩을 끈다.
      // (인증은 settingSources와 무관하게 유지된다.)
      _meta: { claudeCode: { options: { settingSources: [] } } }
    }),
    // ~9.5s cold boot alone; under a Red Team scan several sessions boot at once
    // and contend, so allow more headroom than a single spawn needs.
    25_000,
    'newSession'
  )

  // claude-agent-acp는 세션 권한 모드를 사용자의 ~/.claude/settings.json
  // permissions.defaultMode에서 그대로 가져온다 (_meta options로는 못 바꿈).
  // 'dontAsk' 등이면 권한 요청 없이 MCP 도구가 전부 거부되므로, 요청이 항상
  // 위 requestPermission(auto-approve)으로 오도록 'default'로 강제한다.
  const modes = (result as { modes?: { currentModeId?: string } | null }).modes
  if (modes && modes.currentModeId !== 'default') {
    await connection
      .setSessionMode({ sessionId: result.sessionId, modeId: 'default' })
      .catch((e) => console.error('[acp] setSessionMode failed:', e))
  }

  const modelState = modelStateFromSession(result as Parameters<typeof modelStateFromSession>[0])
  console.log('[acp:newSession] result keys:', Object.keys(result), 'models:', modelState.availableModels.length, 'current:', modelState.currentModelId)
  const entry: SessionEntry = {
    agentDef,
    child,
    connection,
    sessionId: result.sessionId,
    onUpdate: null,
    requestPermission: null,
    dead: false,
    availableModels: modelState.availableModels,
    currentModelId: modelState.currentModelId,
    modelConfigId: modelState.modelConfigId,
    activePrompt: null,
    lastPromptFailed: false
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

  // A previous turn may still be in flight (user hit Stop, then sent a new
  // message before the agent honoured session/cancel). ACP allows one prompt
  // per session at a time — re-cancel and wait (bounded) for it to settle.
  if (entry.activePrompt) {
    await entry.connection.cancel({ sessionId: entry.sessionId }).catch(() => null)
    await Promise.race([
      entry.activePrompt,
      new Promise<void>((resolve) => setTimeout(resolve, 6_000))
    ])
  }

  entry.requestPermission = requestPermission ?? null
  const previousPromptFailed = entry.lastPromptFailed
  // The child can think or run one tool for longer than the renderer's 60s
  // silence watchdog. A live process is not a hang; child exit still rejects.
  const stopPulse = startTurnPulse((notification) => {
    if (entry.dead || entry.child.exitCode != null) return
    onUpdate(notification)
  }, sessionId)

  const runOnce = async (
    promptText: string
  ): Promise<{ stopReason: string; updateCount: number; elapsedMs: number }> => {
    let updateCount = 0
    entry.onUpdate = (n) => {
      updateCount++
      onUpdate(n)
    }
    const t0 = Date.now()
    const p = entry.connection.prompt({
      sessionId: entry.sessionId,
      prompt: [{ type: 'text', text: promptText }]
    })
    entry.activePrompt = p.then(
      () => undefined,
      () => undefined
    )
    const res = await p
    return { stopReason: res.stopReason, updateCount, elapsedMs: Date.now() - t0 }
  }

  try {
    let res = await runOnce(text)
    if (isStaleEmptyTurn({ previousPromptFailed, ...res })) {
      // The user's text is already queued in the agent's conversation (it was
      // pushed before the stale idle was consumed), so nudge rather than
      // re-send it — re-sending makes the model answer the same message twice.
      console.warn(`[ACP ${entry.agentDef.id}] stale empty turn after failed prompt — nudging once`)
      res = await runOnce(STALE_TURN_NUDGE)
    }
    entry.lastPromptFailed = false
    return { stopReason: res.stopReason }
  } catch (e) {
    entry.lastPromptFailed = true
    throw e
  } finally {
    stopPulse()
    entry.activePrompt = null
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
  if (entry.modelConfigId) {
    await entry.connection.setSessionConfigOption({
      sessionId: entry.sessionId,
      configId: entry.modelConfigId,
      value: modelId
    })
    entry.currentModelId = modelId
    console.log(`[acp] model set → ${modelId} (session ${entry.sessionId})`)
    return
  }
  const conn = entry.connection as unknown as {
    unstable_setSessionModel?: (params: { sessionId: string; modelId: string }) => Promise<unknown>
  }
  if (!conn.unstable_setSessionModel) {
    throw new Error('ACP SDK does not expose unstable_setSessionModel on this connection')
  }
  await conn.unstable_setSessionModel({ sessionId: entry.sessionId, modelId })
  entry.currentModelId = modelId
  console.log(`[acp] model set → ${modelId} (session ${entry.sessionId})`)
}
