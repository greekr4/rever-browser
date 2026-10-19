import OpenAI from 'openai'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'
import { randomUUID } from 'node:crypto'

import { startMcpServer } from '../mcp/server'
import { getToolProfile } from '../mcp/tool-profile-state'
import { getApiKey, type ApiProvider } from '../settings'
import { beginDirectTurn, takeDirectTool } from './direct-tool-gate'
import { capToolsForOpenAI, OPENAI_MAX_TOOLS } from './openai-tool-cap'
import { startTurnPulse } from './turn-pulse'

import type { SessionNotification } from '@agentclientprotocol/sdk'
import type {
  ChatCompletionMessageParam,
  ChatCompletionFunctionTool,
  ChatCompletionMessageToolCall
} from 'openai/resources/chat/completions'

// Shared Chat Completions loop for providers that speak the OpenAI wire format
// (OpenAI itself, and xAI Grok at api.x.ai). Both attach with the direct tool
// catalog and the 128-name cap. Each provider keeps its own session map.

export interface ModelInfo {
  modelId: string
  name: string
  description?: string | null
}

export interface ChatCompletionsOptions {
  /** Used in session error strings, e.g. "OpenAI" → "unknown OpenAI session". */
  label: string
  /** Session ids start with this, including the colon. "openai:" / "grok:". */
  sessionPrefix: string
  defaultModel: string
  models: readonly ModelInfo[]
  keyProvider: ApiProvider
  missingKeyError: string
  /** console.warn tag. "openai" keeps the historical "OpenAI max" wording. */
  logTag: string
  baseURL?: string
  timeoutMs?: number
  /** When set, used instead of the stored API key. Grok passes the subscription token. */
  resolveApiKey?: () => Promise<string | null>
}

interface ChatSession {
  messages: ChatCompletionMessageParam[]
  modelId: string
  abort: AbortController | null
  dead: boolean
  /** Names actually passed to mcp.callTool. Refusals never enter this set. */
  dispatched: Set<string>
}

export interface ChatCompletionsProvider {
  spawn: () => Promise<{ sessionId: string }>
  prompt: (
    sessionId: string,
    text: string,
    onUpdate: (n: SessionNotification) => void
  ) => Promise<{ stopReason: string }>
  cancel: (sessionId: string) => Promise<void>
  kill: (sessionId: string) => Promise<void>
  modelState: (
    sessionId: string
  ) => { availableModels: ModelInfo[]; currentModelId: string | null } | null
  setModel: (sessionId: string, modelId: string) => Promise<void>
  isSession: (sessionId: string) => boolean
}

// One direct-catalog MCP client for every Chat Completions provider.
let mcpBridge: Promise<{ client: Client; mapped: ChatCompletionFunctionTool[] }> | null = null

async function getMcpBridge(): Promise<{ client: Client; mapped: ChatCompletionFunctionTool[] }> {
  if (mcpBridge) return mcpBridge
  mcpBridge = (async () => {
    const { url, authHeader } = await startMcpServer()
    const client = new Client({ name: 'rever-direct', version: '0.1.0' })
    await client.connect(
      new StreamableHTTPClientTransport(new URL(url), {
        requestInit: { headers: { Authorization: authHeader, 'X-Rever-Catalog': 'direct' } }
      })
    )
    const listed = await client.listTools()
    const mapped: ChatCompletionFunctionTool[] = listed.tools.map((t) => ({
      type: 'function',
      function: {
        name: t.name,
        description: t.description ?? undefined,
        parameters: t.inputSchema as Record<string, unknown>
      }
    }))
    return { client, mapped }
  })()
  return mcpBridge
}

function emitText(onUpdate: (n: SessionNotification) => void, sessionId: string, text: string): void {
  onUpdate({
    sessionId,
    update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text } }
  } as unknown as SessionNotification)
}

function emitThought(onUpdate: (n: SessionNotification) => void, sessionId: string, text: string): void {
  onUpdate({
    sessionId,
    update: { sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text } }
  } as unknown as SessionNotification)
}

function reasoningText(delta: object | undefined): string {
  if (!delta) return ''
  const extra = delta as { reasoning_content?: unknown; reasoning?: unknown }
  if (typeof extra.reasoning_content === 'string') return extra.reasoning_content
  if (typeof extra.reasoning === 'string') return extra.reasoning
  return ''
}

function extractToolText(result: unknown): string {
  const content = (result as { content?: unknown }).content
  if (!Array.isArray(content)) return ''
  const parts: string[] = []
  for (const c of content) {
    if (c && typeof c === 'object' && (c as { type?: string }).type === 'text') {
      parts.push(String((c as { text?: string }).text ?? ''))
    }
  }
  return parts.join('\n')
}

interface PartialToolCall {
  id: string
  name: string
  args: string
}

function safeParse(json: string): Record<string, unknown> {
  try {
    const v = JSON.parse(json || '{}')
    return v && typeof v === 'object' ? (v as Record<string, unknown>) : {}
  } catch {
    return {}
  }
}

export function createChatCompletionsProvider(opts: ChatCompletionsOptions): ChatCompletionsProvider {
  const sessions = new Map<string, ChatSession>()

  const isSession = (sessionId: string): boolean => sessionId.startsWith(opts.sessionPrefix)

  const requireSession = (sessionId: string): ChatSession => {
    const session = isSession(sessionId) ? sessions.get(sessionId) : undefined
    if (!session) throw new Error(`unknown ${opts.label} session: ${sessionId}`)
    return session
  }

  return {
    isSession,

    async spawn() {
      const sessionId = `${opts.sessionPrefix}${randomUUID()}`
      sessions.set(sessionId, {
        messages: [],
        modelId: opts.defaultModel,
        abort: null,
        dead: false,
        dispatched: new Set()
      })
      return { sessionId }
    },

    async prompt(sessionId, text, onUpdate) {
      const session = requireSession(sessionId)
      if (session.dead) throw new Error(`${opts.label} session is dead: ${sessionId}`)

      const stopPulse = startTurnPulse(onUpdate, sessionId)
      try {
      const apiKey = opts.resolveApiKey ? await opts.resolveApiKey() : getApiKey(opts.keyProvider)
      if (!apiKey) throw new Error(opts.missingKeyError)

      const client = new OpenAI({
        apiKey,
        ...(opts.baseURL ? { baseURL: opts.baseURL } : {}),
        ...(opts.timeoutMs != null ? { timeout: opts.timeoutMs } : {})
      })
      const { client: mcp, mapped } = await getMcpBridge()

      session.messages.push({ role: 'user', content: text })
      const abort = new AbortController()
      session.abort = abort

      try {
        // tool_calls가 없을 때까지 반복하는 수동 에이전트 루프.
        // eslint-disable-next-line no-constant-condition
        while (true) {
          const allowed = beginDirectTurn(getToolProfile(), session.dispatched)
          const filtered = mapped.filter((t) => allowed.has(t.function.name))
          const { tools, dropped } = capToolsForOpenAI(filtered)
          if (dropped.length) {
            const limitNote =
              opts.logTag === 'openai'
                ? `OpenAI max ${OPENAI_MAX_TOOLS}`
                : `Chat Completions max ${OPENAI_MAX_TOOLS}`
            console.warn(
              `[${opts.logTag}] tool list capped ${filtered.length}→${tools.length} (${limitNote}); dropped: ${dropped.join(', ')}`
            )
          }
          const stream = await client.chat.completions.create(
            {
              model: session.modelId,
              messages: session.messages,
              tools,
              stream: true
            },
            { signal: abort.signal }
          )

          let assembledText = ''
          const partials = new Map<number, PartialToolCall>()
          let finishReason: string | null = null

          for await (const chunk of stream) {
            const choice = chunk.choices[0]
            if (!choice) continue
            const delta = choice.delta
            if (delta?.content) {
              assembledText += delta.content
              emitText(onUpdate, sessionId, delta.content)
            }
            const thought = reasoningText(delta)
            if (thought) emitThought(onUpdate, sessionId, thought)
            if (delta?.tool_calls) {
              for (const tc of delta.tool_calls) {
                const existing = partials.get(tc.index) ?? { id: '', name: '', args: '' }
                if (tc.id) existing.id = tc.id
                if (tc.function?.name) existing.name = tc.function.name
                if (tc.function?.arguments) existing.args += tc.function.arguments
                partials.set(tc.index, existing)
              }
            }
            if (choice.finish_reason) finishReason = choice.finish_reason
          }

          if (finishReason !== 'tool_calls' || partials.size === 0) {
            return { stopReason: 'end_turn' }
          }

          const toolCalls: ChatCompletionMessageToolCall[] = [...partials.values()].map((p) => ({
            id: p.id,
            type: 'function',
            function: { name: p.name, arguments: p.args || '{}' }
          }))
          session.messages.push({
            role: 'assistant',
            content: assembledText || null,
            tool_calls: toolCalls
          })

          for (const p of partials.values()) {
            onUpdate({
              sessionId,
              update: {
                sessionUpdate: 'tool_call',
                toolCallId: p.id,
                title: p.name,
                rawInput: safeParse(p.args)
              }
            } as unknown as SessionNotification)

            const decision = takeDirectTool(p.name, allowed, session.dispatched)
            if (!decision.run) {
              onUpdate({
                sessionId,
                update: {
                  sessionUpdate: 'tool_call_update',
                  toolCallId: p.id,
                  status: 'failed',
                  rawOutput: decision.content
                }
              } as unknown as SessionNotification)
              session.messages.push({ role: 'tool', tool_call_id: p.id, content: decision.content })
              continue
            }

            try {
              const result = await mcp.callTool({
                name: p.name,
                arguments: safeParse(p.args)
              })
              const outText = extractToolText(result)
              const isError = (result as { isError?: boolean }).isError === true
              onUpdate({
                sessionId,
                update: {
                  sessionUpdate: 'tool_call_update',
                  toolCallId: p.id,
                  status: isError ? 'failed' : 'completed',
                  rawOutput: outText
                }
              } as unknown as SessionNotification)
              session.messages.push({ role: 'tool', tool_call_id: p.id, content: outText })
            } catch (e) {
              const msg = e instanceof Error ? e.message : String(e)
              onUpdate({
                sessionId,
                update: {
                  sessionUpdate: 'tool_call_update',
                  toolCallId: p.id,
                  status: 'failed',
                  rawOutput: msg
                }
              } as unknown as SessionNotification)
              session.messages.push({ role: 'tool', tool_call_id: p.id, content: msg })
            }
          }
        }
      } finally {
        session.abort = null
      }
      } finally {
        stopPulse()
      }
    },

    async cancel(sessionId) {
      const session = isSession(sessionId) ? sessions.get(sessionId) : undefined
      session?.abort?.abort()
    },

    async kill(sessionId) {
      const session = isSession(sessionId) ? sessions.get(sessionId) : undefined
      if (!session) return
      session.abort?.abort()
      session.dead = true
      sessions.delete(sessionId)
    },

    modelState(sessionId) {
      const session = isSession(sessionId) ? sessions.get(sessionId) : undefined
      if (!session) return null
      return { availableModels: [...opts.models], currentModelId: session.modelId }
    },

    async setModel(sessionId, modelId) {
      const session = requireSession(sessionId)
      session.modelId = modelId
    }
  }
}
