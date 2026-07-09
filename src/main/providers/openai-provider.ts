import OpenAI from 'openai'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'
import { randomUUID } from 'node:crypto'

import { startMcpServer } from '../mcp/server'
import { getApiKey } from '../settings'

import type { SessionNotification } from '@agentclientprotocol/sdk'
import type {
  ChatCompletionMessageParam,
  ChatCompletionTool,
  ChatCompletionMessageToolCall
} from 'openai/resources/chat/completions'

// OpenAI Chat Completions API를 직접 호출하는 provider. 구조는 anthropic-provider와
// 동일하다 — 인프로세스 MCP 서버(localhost)에 MCP 클라이언트로 붙어 도구를
// function tool로 노출하고, tool_calls가 나오면 client-side로 실행한다.

interface ModelInfo {
  modelId: string
  name: string
  description?: string | null
}

export const OPENAI_MODELS: ModelInfo[] = [
  { modelId: 'gpt-4o', name: 'GPT-4o', description: 'Flagship multimodal' },
  { modelId: 'gpt-4o-mini', name: 'GPT-4o mini', description: 'Fast, cheap' },
  { modelId: 'o3', name: 'o3', description: 'Reasoning' },
  { modelId: 'o4-mini', name: 'o4-mini', description: 'Fast reasoning' }
]

const DEFAULT_MODEL = 'gpt-4o'

interface OpenAiSession {
  messages: ChatCompletionMessageParam[]
  modelId: string
  abort: AbortController | null
  dead: boolean
}

const sessions = new Map<string, OpenAiSession>()

// MCP 클라이언트/도구는 프로세스당 한 번만 연결한다.
let mcpBridge: Promise<{ client: Client; tools: ChatCompletionTool[] }> | null = null

async function getMcpBridge(): Promise<{ client: Client; tools: ChatCompletionTool[] }> {
  if (mcpBridge) return mcpBridge
  mcpBridge = (async () => {
    const { url } = await startMcpServer()
    const client = new Client({ name: 'rever-openai', version: '0.1.0' })
    await client.connect(new StreamableHTTPClientTransport(new URL(url)))
    const listed = await client.listTools()
    const tools: ChatCompletionTool[] = listed.tools.map((t) => ({
      type: 'function',
      function: {
        name: t.name,
        description: t.description ?? undefined,
        parameters: t.inputSchema as Record<string, unknown>
      }
    }))
    return { client, tools }
  })()
  return mcpBridge
}

export async function spawnOpenAiSession(): Promise<{ sessionId: string }> {
  const sessionId = `openai:${randomUUID()}`
  sessions.set(sessionId, {
    messages: [],
    modelId: DEFAULT_MODEL,
    abort: null,
    dead: false
  })
  return { sessionId }
}

function emitText(onUpdate: (n: SessionNotification) => void, sessionId: string, text: string): void {
  onUpdate({
    sessionId,
    update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text } }
  } as unknown as SessionNotification)
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

// 스트리밍 중 index별로 조각나 오는 tool_call delta를 누적한다.
interface PartialToolCall {
  id: string
  name: string
  args: string
}

export async function promptOpenAiSession(
  sessionId: string,
  text: string,
  onUpdate: (n: SessionNotification) => void
): Promise<{ stopReason: string }> {
  const session = sessions.get(sessionId)
  if (!session) throw new Error(`unknown OpenAI session: ${sessionId}`)
  if (session.dead) throw new Error(`OpenAI session is dead: ${sessionId}`)

  const apiKey = getApiKey('openai')
  if (!apiKey) {
    throw new Error('No OpenAI API key set. Add one in settings (OpenAI API key).')
  }

  const openai = new OpenAI({ apiKey })
  const { client: mcp, tools } = await getMcpBridge()

  session.messages.push({ role: 'user', content: text })
  const abort = new AbortController()
  session.abort = abort

  try {
    // tool_calls가 없을 때까지 반복하는 수동 에이전트 루프.
    // eslint-disable-next-line no-constant-condition
    while (true) {
      const stream = await openai.chat.completions.create(
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

      // assistant 턴을 tool_calls와 함께 히스토리에 넣는다.
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
}

function safeParse(json: string): Record<string, unknown> {
  try {
    const v = JSON.parse(json || '{}')
    return v && typeof v === 'object' ? (v as Record<string, unknown>) : {}
  } catch {
    return {}
  }
}

export async function cancelOpenAiSession(sessionId: string): Promise<void> {
  const session = sessions.get(sessionId)
  session?.abort?.abort()
}

export async function killOpenAiSession(sessionId: string): Promise<void> {
  const session = sessions.get(sessionId)
  if (!session) return
  session.abort?.abort()
  session.dead = true
  sessions.delete(sessionId)
}

export function getOpenAiModelState(
  sessionId: string
): { availableModels: ModelInfo[]; currentModelId: string | null } | null {
  const session = sessions.get(sessionId)
  if (!session) return null
  return { availableModels: OPENAI_MODELS, currentModelId: session.modelId }
}

export async function setOpenAiModel(sessionId: string, modelId: string): Promise<void> {
  const session = sessions.get(sessionId)
  if (!session) throw new Error(`unknown OpenAI session: ${sessionId}`)
  session.modelId = modelId
}

export function isOpenAiSession(sessionId: string): boolean {
  return sessionId.startsWith('openai:')
}
