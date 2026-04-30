import SYSTEM_PROMPT from '@/ai/system-prompt.md?raw'

import { mapUpdate } from './acp-map-update'

import type { ChatTransport, UIMessage, UIMessageChunk } from 'ai'
import type { ACPAgentDef } from '@/constants'

export function formatConnectionError(e: unknown): string {
  const msg = e instanceof Error ? e.message : String(e)
  if (msg.includes('ENOENT') || msg.includes('spawn')) {
    return `Agent binary not found. Check your PATH. (${msg})`
  }
  if (msg.includes('timeout') || msg.includes('Timeout')) {
    return 'ACP server did not respond in time.'
  }
  return msg
}

export class ACPChatTransport implements ChatTransport<UIMessage> {
  private sessionId: string | null = null
  private agentDef: ACPAgentDef
  private cwd: string
  private systemPrompt: string
  private sentContext = false

  constructor(options: { agentDef: ACPAgentDef; cwd?: string; systemPrompt?: string }) {
    this.agentDef = options.agentDef
    this.cwd = options.cwd ?? '.'
    this.systemPrompt = options.systemPrompt ?? SYSTEM_PROMPT
  }

  /**
   * Kill the current ACP session and clear local context flags so the next
   * sendMessages spawns a fresh agent with no prior conversation memory.
   */
  async reset(): Promise<void> {
    const id = this.sessionId
    this.sessionId = null
    this.sentContext = false
    if (id) {
      try {
        await window.rev.acp.kill(id)
      } catch (e) {
        console.warn('[acp] kill failed during reset:', e)
      }
    }
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

    if (!this.sessionId) {
      const { sessionId } = await window.rev.acp.spawn(
        {
          id: this.agentDef.id,
          command: this.agentDef.command,
          args: this.agentDef.args
        },
        this.cwd
      )
      this.sessionId = sessionId
    }

    const promptText = this.sentContext ? text : `${this.systemPrompt}\n\n${text}`
    this.sentContext = true
    const sessionId = this.sessionId

    return new ReadableStream<UIMessageChunk>({
      start: (controller) => {
        const textId = `text-${Date.now()}`
        let textStarted = false
        let closed = false
        const seenToolCallIds = new Set<string>()

        const finish = (
          reason: 'stop' | 'other' | 'error',
          errorText?: string
        ) => {
          if (closed) return
          closed = true
          if (errorText) controller.enqueue({ type: 'error', errorText })
          if (textStarted) controller.enqueue({ type: 'text-end', id: textId })
          controller.enqueue({ type: 'finish-step' })
          controller.enqueue({ type: 'finish', finishReason: reason })
          controller.close()
        }

        abortSignal?.addEventListener('abort', () => {
          void window.rev.acp.cancel(sessionId)
          finish('stop')
        })

        controller.enqueue({ type: 'start' })
        controller.enqueue({ type: 'start-step' })

        window.rev.acp
          .prompt(sessionId, promptText, (notification) => {
            if (closed) return
            const update = (notification as unknown as { update: Parameters<typeof mapUpdate>[0] }).update
            const result = mapUpdate(update, textId, textStarted, { seenToolCallIds })
            for (const chunk of result.chunks) controller.enqueue(chunk)
            textStarted = result.textStarted
          })
          .then((res) => {
            finish(res.stopReason === 'end_turn' ? 'stop' : 'other')
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
    if (this.sessionId) {
      await window.rev.acp.kill(this.sessionId).catch(() => null)
      this.sessionId = null
    }
  }

}
