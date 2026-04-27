import { ClientSideConnection, ndJsonStream, PROTOCOL_VERSION } from '@agentclientprotocol/sdk'

import SYSTEM_PROMPT from '@/ai/system-prompt.md?raw'

import { mapUpdate } from './acp-map-update'

import type {
  Client,
  Agent,
  SessionNotification,
  RequestPermissionRequest,
  RequestPermissionResponse
} from '@agentclientprotocol/sdk'
import type { ChatTransport, UIMessage, UIMessageChunk } from 'ai'
import type { ACPAgentDef } from '@/constants'

type TauriChild = {
  write(data: number[]): Promise<void>
  kill(): Promise<void>
}

interface ACPSession {
  connection: ClientSideConnection
  sessionId: string
  child: TauriChild
  onUpdate: ((params: SessionNotification) => void) | null
  dead: boolean
}

export function formatConnectionError(e: unknown): string {
  const msg = e instanceof Error ? e.message : String(e)
  if (
    msg.includes('ECONNREFUSED') ||
    msg.includes('fetch failed') ||
    msg.includes('Failed to fetch')
  ) {
    return 'MCP server is not running. Make sure the editor is open.'
  }
  if (msg.includes('timeout') || msg.includes('Timeout') || msg.includes('ETIMEDOUT')) {
    return 'MCP server did not respond in time.'
  }
  return msg
}

interface ACPDebugEntry {
  ts: number
  type: string
  data: unknown
}

const MAX_LOG_AGE_MS = 5 * 60 * 1000
const ACP_TURN_MAX_MS = 0
const ACP_TURN_STALL_MS = 0
const IS_DEV = import.meta.env.DEV

export const acpDebugLog: ACPDebugEntry[] = []

function pruneOldEntries() {
  const cutoff = Date.now() - MAX_LOG_AGE_MS
  while (acpDebugLog.length > 0 && acpDebugLog[0].ts < cutoff) {
    acpDebugLog.shift()
  }
}

export function getAcpDebugText(): string {
  pruneOldEntries()
  return acpDebugLog
    .map((e) => `[${new Date(e.ts).toISOString()}] ${e.type}\n${JSON.stringify(e.data, null, 2)}`)
    .join('\n\n---\n\n')
}

export function clearAcpDebugLog() {
  acpDebugLog.length = 0
}

export class ACPChatTransport implements ChatTransport<UIMessage> {
  private session: ACPSession | null = null
  private agentDef: ACPAgentDef
  private cwd: string
  private systemPrompt: string
  private sessionMeta: Record<string, unknown> | null
  private sentContext = false
  private destroying = false

  constructor(options: {
    agentDef: ACPAgentDef
    cwd?: string
    systemPrompt?: string
    sessionMeta?: Record<string, unknown> | null
  }) {
    this.agentDef = options.agentDef
    this.cwd = options.cwd ?? '.'
    this.systemPrompt = options.systemPrompt ?? SYSTEM_PROMPT
    this.sessionMeta = options.sessionMeta ?? null
  }

  async sendMessages({
    messages,
    abortSignal
  }: Parameters<ChatTransport<UIMessage>['sendMessages']>[0]): Promise<
    ReadableStream<UIMessageChunk>
  > {
    const lastUserMessage = [...messages].reverse().find((m) => m.role === 'user')
    const text =
      lastUserMessage?.parts
        .filter((p): p is { type: 'text'; text: string } => p.type === 'text')
        .map((p) => p.text)
        .join('\n') ?? ''

    if (this.session?.dead) {
      this.session = null
    }

    if (!this.session) {
      this.session = await this.spawnAgent()
    }

    const promptText = this.sentContext ? text : `${this.systemPrompt}\n\n${text}`
    this.sentContext = true

    const { connection, sessionId } = this.session
    const session = this.session

    return new ReadableStream<UIMessageChunk>({
      start: (controller) => {
        const textId = `text-${Date.now()}`
        let textStarted = false
        let closed = false
        const seenToolCallIds = new Set<string>()
        let maxTimer: ReturnType<typeof setTimeout> | null = null
        let stallTimer: ReturnType<typeof setTimeout> | null = null

        function clearTimers() {
          if (maxTimer) {
            clearTimeout(maxTimer)
            maxTimer = null
          }
          if (stallTimer) {
            clearTimeout(stallTimer)
            stallTimer = null
          }
        }

        function resetStallTimer() {
          if (ACP_TURN_STALL_MS <= 0) return
          if (stallTimer) clearTimeout(stallTimer)
          stallTimer = setTimeout(() => {
            void connection.cancel({ sessionId }).catch(() => null)
            finish('other', 'ACP 응답 지연으로 요청을 종료했습니다.')
          }, ACP_TURN_STALL_MS)
        }

        function finish(reason: 'stop' | 'other' | 'error', errorText?: string) {
          if (closed) return
          closed = true
          clearTimers()
          if (errorText) controller.enqueue({ type: 'error', errorText })
          if (textStarted) controller.enqueue({ type: 'text-end', id: textId })
          controller.enqueue({ type: 'finish-step' })
          controller.enqueue({ type: 'finish', finishReason: reason })
          session.onUpdate = null
          controller.close()
        }

        session.onUpdate = (params) => {
          if (closed) return
          if (IS_DEV) {
            acpDebugLog.push({
              ts: Date.now(),
              type: params.update.sessionUpdate,
              data: params.update
            })
          }
          resetStallTimer()
          const result = mapUpdate(params.update, textId, textStarted, {
            seenToolCallIds
          })
          for (const chunk of result.chunks) {
            controller.enqueue(chunk)
          }
          textStarted = result.textStarted
        }

        abortSignal?.addEventListener('abort', () => {
          void connection.cancel({ sessionId })
          finish('stop')
        })

        controller.enqueue({ type: 'start' })
        controller.enqueue({ type: 'start-step' })
        if (ACP_TURN_MAX_MS > 0) {
          maxTimer = setTimeout(() => {
            void connection.cancel({ sessionId }).catch(() => null)
            finish('other', 'ACP 최대 실행 시간을 초과해 요청을 종료했습니다.')
          }, ACP_TURN_MAX_MS)
        }
        resetStallTimer()

        connection
          .prompt({
            sessionId,
            prompt: [{ type: 'text', text: promptText }]
          })
          .then((result) => {
            finish(result.stopReason === 'end_turn' ? 'stop' : 'other')
          })
          .catch((e) => {
            finish('error', formatConnectionError(e))
          })
      }
    })
  }

  async reconnectToStream(): Promise<ReadableStream<UIMessageChunk> | null> {
    return null
  }

  async destroy(): Promise<void> {
    this.destroying = true
    if (this.session) {
      await this.session.child.kill()
      this.session = null
    }
  }

  private async spawnAgent(): Promise<ACPSession> {
    const { Command } = await import('@tauri-apps/plugin-shell')

    const command = Command.create(this.agentDef.command, this.agentDef.args, {
      encoding: 'raw'
    })

    const stdoutChunks: Uint8Array[] = []
    let stdoutResolver: ((chunk: Uint8Array) => void) | null = null

    command.stdout.on('data', (raw: Uint8Array | number[]) => {
      const chunk = raw instanceof Uint8Array ? raw : new Uint8Array(raw)
      if (stdoutResolver) {
        const resolve = stdoutResolver
        stdoutResolver = null
        resolve(chunk)
      } else {
        stdoutChunks.push(chunk)
      }
    })

    command.stderr.on('data', (raw: Uint8Array | number[] | string) => {
      const text =
        typeof raw === 'string'
          ? raw
          : new TextDecoder().decode(raw instanceof Uint8Array ? raw : new Uint8Array(raw))
      console.error(`[ACP ${this.agentDef.id}]`, text)
    })

    command.on('close', () => {
      if (this.destroying || !this.session) return
      this.session.dead = true
      this.session = null
    })

    const child = await command.spawn()

    const output = new ReadableStream<Uint8Array>({
      async pull(controller) {
        const buffered = stdoutChunks.shift()
        if (buffered) {
          controller.enqueue(buffered)
          return
        }
        await new Promise<void>((resolve) => {
          stdoutResolver = (chunk) => {
            controller.enqueue(chunk)
            resolve()
          }
        })
      }
    })

    const input = new WritableStream<Uint8Array>({
      async write(chunk) {
        await child.write(Array.from(chunk))
      }
    })

    const stream = ndJsonStream(input, output)
    let onUpdate: ACPSession['onUpdate'] = null

    const clientImpl: Client = {
      async requestPermission(
        params: RequestPermissionRequest
      ): Promise<RequestPermissionResponse> {
        const { requestPermissionFromUser } = await import('@/ai/acp-permission')
        return requestPermissionFromUser(params)
      },

      async sessionUpdate(params: SessionNotification): Promise<void> {
        onUpdate?.(params)
      }
    }

    const connection = new ClientSideConnection((_agent: Agent) => clientImpl, stream)

    await connection.initialize({
      protocolVersion: PROTOCOL_VERSION,
      clientCapabilities: {}
    })

    let sessionResult
    try {
      // M0: no MCP server yet — added in M0.4 (browser/network tools).
      sessionResult = await connection.newSession({
        cwd: this.cwd,
        mcpServers: [],
        _meta: this.sessionMeta
      })

      if (this.agentDef.id === 'gemini-cli') {
        const modes = sessionResult.modes?.availableModes ?? []
        const preferredModeId =
          modes.find((m) => m.id === 'yolo')?.id ?? modes.find((m) => m.id === 'autoEdit')?.id
        if (preferredModeId && sessionResult.modes?.currentModeId !== preferredModeId) {
          await connection.setSessionMode({
            sessionId: sessionResult.sessionId,
            modeId: preferredModeId
          })
        }
      }
    } catch (e) {
      await child.kill()
      throw new Error(formatConnectionError(e))
    }

    const session: ACPSession = {
      connection,
      sessionId: sessionResult.sessionId,
      child,
      dead: false,
      get onUpdate() {
        return onUpdate
      },
      set onUpdate(fn) {
        onUpdate = fn
      }
    }

    return session
  }
}
