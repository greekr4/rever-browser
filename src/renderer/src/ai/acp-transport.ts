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

  getSessionId(): string | null {
    return this.sessionId
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

    const alreadySentContext = this.sentContext
    const promptText = alreadySentContext ? text : `${this.systemPrompt}\n\n${text}`
    // self 참조: ReadableStream start 콜백은 arrow function이 아니므로 this를 미리 캡처
    // eslint-disable-next-line @typescript-eslint/no-this-alias
    const self = this
    const sessionId = this.sessionId

    return new ReadableStream<UIMessageChunk>({
      start: (controller) => {
        const textId = `text-${Date.now()}`
        let textStarted = false
        let closed = false
        let anyContent = false
        const seenToolCallIds = new Set<string>()

        // Watchdog: if 60s pass with NO notification at all, surface as error
        // so the chat status doesn't get stuck on "streaming" forever.
        let watchdog: ReturnType<typeof setTimeout> | null = null
        const armWatchdog = () => {
          if (watchdog) clearTimeout(watchdog)
          watchdog = setTimeout(() => {
            if (closed) return
            void window.rev.acp.cancel(sessionId).catch(() => null)
            finish('error', 'Agent went silent for 60s — cancelled. Try /reset if it persists.')
          }, 60_000)
        }
        const clearWatchdog = () => {
          if (watchdog) {
            clearTimeout(watchdog)
            watchdog = null
          }
        }

        const finish = (
          reason: 'stop' | 'other' | 'error',
          errorText?: string
        ) => {
          if (closed) return
          closed = true
          clearWatchdog()
          if (errorText) controller.enqueue({ type: 'error', errorText })
          if (textStarted) controller.enqueue({ type: 'text-end', id: textId })
          // Surface empty-turn case so users don't think the agent ignored them.
          if (!errorText && !anyContent) {
            const note = `text-${Date.now()}-empty`
            controller.enqueue({ type: 'text-start', id: note })
            controller.enqueue({
              type: 'text-delta',
              id: note,
              delta: '_(agent returned no response — check stderr or try /reset)_'
            })
            controller.enqueue({ type: 'text-end', id: note })
          }
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
        armWatchdog()

        window.rev.acp
          .prompt(sessionId, promptText, (notification) => {
            if (closed) return
            armWatchdog()
            const update = (notification as unknown as { update: Parameters<typeof mapUpdate>[0] }).update
            const result = mapUpdate(update, textId, textStarted, { seenToolCallIds })
            if (result.chunks.length > 0) anyContent = true
            for (const chunk of result.chunks) controller.enqueue(chunk)
            textStarted = result.textStarted
          })
          .then((res) => {
            // 프롬프트가 성공적으로 완료된 후에 sentContext를 true로 설정
            // (실패 시에는 시스템 프롬프트가 다음 시도에서 재전송됨)
            if (!alreadySentContext) self.sentContext = true
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
