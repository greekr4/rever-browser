import type { SessionUpdate } from '@agentclientprotocol/sdk'
import type { UIMessageChunk } from 'ai'

import { detectSafeguardNotice } from './provider-notice'

export interface TurnState {
  textStarted: boolean
  /** A reasoning run is open between a start and its close. */
  reasoningStarted: boolean
  /** How many reasoning runs opened so far this turn; ids must be unique. */
  reasoningRuns: number
}

interface MapUpdateOptions {
  seenToolCallIds?: Set<string>
}

/**
 * Streamed turn state threaded through mapUpdate (mutated in place) so
 * consecutive deltas extend open parts instead of each opening its own.
 */
export function newTurnState(): TurnState {
  return { textStarted: false, reasoningStarted: false, reasoningRuns: 0 }
}

export function mapUpdate(
  update: SessionUpdate,
  textId: string,
  state: TurnState,
  options: MapUpdateOptions = {}
): UIMessageChunk[] {
  const chunks: UIMessageChunk[] = []
  const seenToolCallIds = options.seenToolCallIds

  // GLM/Grok stream reasoning token-by-token: a part per delta would explode
  // into dozens of "thoughts" blocks. One part per run of consecutive deltas.
  const runId = () => `reasoning-${textId}-${state.reasoningRuns}`
  const closeReasoning = () => {
    if (!state.reasoningStarted) return
    state.reasoningStarted = false
    chunks.push({ type: 'reasoning-end', id: runId() })
  }

  switch (update.sessionUpdate) {
    case 'agent_message_chunk': {
      closeReasoning()
      if (update.content.type === 'text' && update.content.text) {
        if (!state.textStarted) {
          chunks.push({ type: 'text-start', id: textId })
          state.textStarted = true
        }
        // The SDK streams its safeguard / model-fallback notice glued to the
        // adjacent answer (no surrounding newlines), so it renders as one run-on
        // paragraph. Isolate a notice chunk onto its own paragraph.
        // shortcut: only catches a notice that arrives as one chunk; if the SDK
        // splits it across deltas, upgrade to buffering in the transport.
        const text = detectSafeguardNotice(update.content.text)
          ? `\n\n${update.content.text.trim()}\n\n`
          : update.content.text
        chunks.push({
          type: 'text-delta',
          id: textId,
          delta: text
        })
      } else if (update.content.type !== 'text') {
        console.warn('[ACP] Unhandled content type:', update.content.type)
      }
      break
    }
    case 'agent_thought_chunk': {
      if (update.content.type === 'text') {
        if (!state.reasoningStarted) {
          state.reasoningStarted = true
          state.reasoningRuns += 1
          chunks.push({ type: 'reasoning-start', id: runId() })
        }
        chunks.push({
          type: 'reasoning-delta',
          id: runId(),
          delta: update.content.text
        })
      }
      break
    }
    case 'tool_call': {
      closeReasoning()
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
      closeReasoning()
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
    default:
      // rever_pulse and any other activity notice: keep the turn alive, show
      // nothing. Deliberately does NOT close an open reasoning part — pulses
      // interleave with thought deltas on quiet turns.
      break
  }

  return chunks
}

/** Close the turn's open reasoning part, if any (call at turn end). */
export function endOpenReasoning(textId: string, state: TurnState): UIMessageChunk[] {
  if (!state.reasoningStarted) return []
  state.reasoningStarted = false
  return [{ type: 'reasoning-end', id: `reasoning-${textId}-${state.reasoningRuns}` }]
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
