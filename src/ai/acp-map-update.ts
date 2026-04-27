import type { SessionUpdate } from '@agentclientprotocol/sdk'
import type { UIMessageChunk } from 'ai'

export interface MapResult {
  chunks: UIMessageChunk[]
  textStarted: boolean
}

interface MapUpdateOptions {
  seenToolCallIds?: Set<string>
}

export function mapUpdate(
  update: SessionUpdate,
  textId: string,
  textStarted: boolean,
  options: MapUpdateOptions = {}
): MapResult {
  const chunks: UIMessageChunk[] = []
  const seenToolCallIds = options.seenToolCallIds

  switch (update.sessionUpdate) {
    case 'agent_message_chunk': {
      if (update.content.type === 'text' && update.content.text) {
        if (!textStarted) {
          chunks.push({ type: 'text-start', id: textId })
          textStarted = true
        }
        chunks.push({
          type: 'text-delta',
          id: textId,
          delta: update.content.text
        })
      } else if (update.content.type !== 'text') {
        console.warn('[ACP] Unhandled content type:', update.content.type)
      }
      break
    }
    case 'agent_thought_chunk': {
      if (update.content.type === 'text') {
        const rid = `reasoning-${textId}`
        chunks.push({ type: 'reasoning-start', id: rid })
        chunks.push({
          type: 'reasoning-delta',
          id: rid,
          delta: update.content.text
        })
        chunks.push({ type: 'reasoning-end', id: rid })
      }
      break
    }
    case 'tool_call': {
      if (!update.title) {
        console.warn('[ACP] Tool call without title:', update.toolCallId)
      }
      const toolName = update.title || 'unknown'
      seenToolCallIds?.add(update.toolCallId)
      chunks.push({
        type: 'tool-input-start',
        toolCallId: update.toolCallId,
        toolName,
        providerExecuted: true,
        title: update.title
      })
      if (update.rawInput) {
        chunks.push({
          type: 'tool-input-available',
          toolCallId: update.toolCallId,
          toolName,
          input: update.rawInput,
          providerExecuted: true,
          title: update.title
        })
      }
      break
    }
    case 'tool_call_update': {
      const toolName = update.title || 'unknown'
      const seen = seenToolCallIds?.has(update.toolCallId) ?? false
      if (!seen) {
        seenToolCallIds?.add(update.toolCallId)
        chunks.push({
          type: 'tool-input-start',
          toolCallId: update.toolCallId,
          toolName,
          providerExecuted: true,
          title: update.title ?? undefined
        })
        if (typeof update.rawInput !== 'undefined') {
          chunks.push({
            type: 'tool-input-available',
            toolCallId: update.toolCallId,
            toolName,
            input: update.rawInput,
            providerExecuted: true,
            title: update.title ?? undefined
          })
        }
      }

      if (update.status === 'completed') {
        chunks.push({
          type: 'tool-output-available',
          toolCallId: update.toolCallId,
          output: update.rawOutput ?? textFromContent(update.content ?? undefined),
          providerExecuted: true
        })
      } else if (update.status === 'failed') {
        chunks.push({
          type: 'tool-output-error',
          toolCallId: update.toolCallId,
          errorText: textFromContent(update.content ?? undefined) ?? 'Tool call failed',
          providerExecuted: true
        })
      } else if (typeof update.rawInput !== 'undefined') {
        chunks.push({
          type: 'tool-input-available',
          toolCallId: update.toolCallId,
          toolName,
          input: update.rawInput,
          providerExecuted: true,
          title: update.title ?? undefined
        })
      }
      break
    }
  }

  return { chunks, textStarted }
}

export function textFromContent(
  content: Record<string, unknown>[] | undefined
): string | undefined {
  if (!content) return undefined
  const parts: string[] = []
  for (const c of content) {
    if (c.type !== 'content') continue
    const inner = c.content as Record<string, unknown> | undefined
    if (inner?.type === 'text' && typeof inner.text === 'string') {
      parts.push(inner.text)
    }
  }
  return parts.length > 0 ? parts.join('\n') : undefined
}
