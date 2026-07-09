import Anthropic from '@anthropic-ai/sdk'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'
import { randomUUID } from 'node:crypto'

import { startMcpServer } from '../mcp/server'
import { getApiKey } from '../settings'

import type { SessionNotification } from '@agentclientprotocol/sdk'

// Anthropic Messages API를 직접 호출하는 provider. ACP(claude-agent-acp) 대신
// 인프로세스에서 에이전트 루프를 돌린다. 브라우저/트래픽 도구는 기존 인프로세스
// MCP 서버(localhost)에 MCP 클라이언트로 붙어 client-side로 실행한다 — MCP
// connector(클라우드가 서버에 접속)는 localhost에 도달할 수 없어 쓸 수 없다.

interface ModelInfo {
  modelId: string
  name: string
  description?: string | null
}

// 심사위원이 그 자리에서 고를 수 있도록 대표 모델만 노출한다.
export const ANTHROPIC_MODELS: ModelInfo[] = [
  { modelId: 'claude-opus-4-8', name: 'Claude Opus 4.8', description: 'Most capable Opus tier' },
  { modelId: 'claude-sonnet-5', name: 'Claude Sonnet 5', description: 'Fast, near-Opus quality' },
  { modelId: 'claude-haiku-4-5', name: 'Claude Haiku 4.5', description: 'Fastest, cheapest' },
  { modelId: 'claude-fable-5', name: 'Claude Fable 5', description: 'Most capable (premium)' }
]

const DEFAULT_MODEL = 'claude-opus-4-8'

// adaptive thinking을 지원하지 않는 모델은 thinking 파라미터를 생략한다.
const NO_ADAPTIVE_THINKING = new Set(['claude-haiku-4-5'])

interface AnthropicSession {
  messages: Anthropic.MessageParam[]
  modelId: string
  abort: AbortController | null
  dead: boolean
}

const sessions = new Map<string, AnthropicSession>()

// MCP 클라이언트/도구는 프로세스당 한 번만 연결한다.
let mcpBridge: Promise<{ client: Client; tools: Anthropic.Tool[] }> | null = null

async function getMcpBridge(): Promise<{ client: Client; tools: Anthropic.Tool[] }> {
  if (mcpBridge) return mcpBridge
  mcpBridge = (async () => {
    const { url } = await startMcpServer()
    const client = new Client({ name: 'rever-anthropic', version: '0.1.0' })
    await client.connect(new StreamableHTTPClientTransport(new URL(url)))
    const listed = await client.listTools()
    const tools: Anthropic.Tool[] = listed.tools.map((t) => ({
      name: t.name,
      description: t.description ?? undefined,
      input_schema: t.inputSchema as Anthropic.Tool.InputSchema
    }))
    return { client, tools }
  })()
  return mcpBridge
}

export async function spawnAnthropicSession(): Promise<{ sessionId: string }> {
  const sessionId = `anthropic:${randomUUID()}`
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

function emitThought(
  onUpdate: (n: SessionNotification) => void,
  sessionId: string,
  text: string
): void {
  onUpdate({
    sessionId,
    update: { sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text } }
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

export async function promptAnthropicSession(
  sessionId: string,
  text: string,
  onUpdate: (n: SessionNotification) => void
): Promise<{ stopReason: string }> {
  const session = sessions.get(sessionId)
  if (!session) throw new Error(`unknown Anthropic session: ${sessionId}`)
  if (session.dead) throw new Error(`Anthropic session is dead: ${sessionId}`)

  const apiKey = getApiKey('anthropic')
  if (!apiKey) {
    throw new Error('No Anthropic API key set. Add one in settings (Anthropic API key).')
  }

  const anthropic = new Anthropic({ apiKey })
  const { client: mcp, tools } = await getMcpBridge()

  session.messages.push({ role: 'user', content: text })
  const abort = new AbortController()
  session.abort = abort

  const thinking = NO_ADAPTIVE_THINKING.has(session.modelId)
    ? undefined
    : ({ type: 'adaptive' } as const)

  try {
    // 툴 호출이 없을 때까지 반복하는 수동 에이전트 루프.
    // 매 턴 스트리밍으로 텍스트/생각을 흘려보내고, tool_use가 있으면 MCP로
    // 실행한 뒤 결과를 다음 턴에 넣어 다시 호출한다.
    // eslint-disable-next-line no-constant-condition
    while (true) {
      const stream = anthropic.messages.stream(
        {
          model: session.modelId,
          max_tokens: 32000,
          tools,
          messages: session.messages,
          ...(thinking ? { thinking } : {})
        },
        { signal: abort.signal }
      )

      for await (const event of stream) {
        if (event.type === 'content_block_delta') {
          if (event.delta.type === 'text_delta' && event.delta.text) {
            emitText(onUpdate, sessionId, event.delta.text)
          } else if (event.delta.type === 'thinking_delta' && event.delta.thinking) {
            emitThought(onUpdate, sessionId, event.delta.thinking)
          }
        }
      }

      const message = await stream.finalMessage()
      session.messages.push({ role: 'assistant', content: message.content })

      if (message.stop_reason !== 'tool_use') {
        return { stopReason: 'end_turn' }
      }

      const toolResults: Anthropic.ToolResultBlockParam[] = []
      for (const block of message.content) {
        if (block.type !== 'tool_use') continue
        // tool_call 알림 — 렌더러의 mapUpdate가 소비하는 형식.
        onUpdate({
          sessionId,
          update: {
            sessionUpdate: 'tool_call',
            toolCallId: block.id,
            title: block.name,
            rawInput: block.input
          }
        } as unknown as SessionNotification)

        try {
          const result = await mcp.callTool({
            name: block.name,
            arguments: block.input as Record<string, unknown>
          })
          const outText = extractToolText(result)
          const isError = (result as { isError?: boolean }).isError === true
          onUpdate({
            sessionId,
            update: {
              sessionUpdate: 'tool_call_update',
              toolCallId: block.id,
              status: isError ? 'failed' : 'completed',
              rawOutput: outText
            }
          } as unknown as SessionNotification)
          toolResults.push({
            type: 'tool_result',
            tool_use_id: block.id,
            content: outText,
            is_error: isError
          })
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e)
          onUpdate({
            sessionId,
            update: {
              sessionUpdate: 'tool_call_update',
              toolCallId: block.id,
              status: 'failed',
              rawOutput: msg
            }
          } as unknown as SessionNotification)
          toolResults.push({
            type: 'tool_result',
            tool_use_id: block.id,
            content: msg,
            is_error: true
          })
        }
      }

      session.messages.push({ role: 'user', content: toolResults })
    }
  } finally {
    session.abort = null
  }
}

export async function cancelAnthropicSession(sessionId: string): Promise<void> {
  const session = sessions.get(sessionId)
  session?.abort?.abort()
}

export async function killAnthropicSession(sessionId: string): Promise<void> {
  const session = sessions.get(sessionId)
  if (!session) return
  session.abort?.abort()
  session.dead = true
  sessions.delete(sessionId)
}

export function getAnthropicModelState(
  sessionId: string
): { availableModels: ModelInfo[]; currentModelId: string | null } | null {
  const session = sessions.get(sessionId)
  if (!session) return null
  return { availableModels: ANTHROPIC_MODELS, currentModelId: session.modelId }
}

export async function setAnthropicModel(sessionId: string, modelId: string): Promise<void> {
  const session = sessions.get(sessionId)
  if (!session) throw new Error(`unknown Anthropic session: ${sessionId}`)
  session.modelId = modelId
}

export function isAnthropicSession(sessionId: string): boolean {
  return sessionId.startsWith('anthropic:')
}
