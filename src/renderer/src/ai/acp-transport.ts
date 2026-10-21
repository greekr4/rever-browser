import SYSTEM_PROMPT from '@/ai/system-prompt.md?raw'
import { toolProfileTurnLine, type ToolProfileId } from '../../../shared/tool-profile-line'
import { cancelPendingPermissions, hasPendingPermission } from './acp-permission'

import { endsBackgroundTurn, startsBackgroundTurn } from './acp-background'
import { mapUpdate } from './acp-map-update'
import { formatConnectionError } from './format-error'
import { detectSafeguardNotice } from './provider-notice'
import { t } from '@/stores/i18n'

import type { ChatTransport, UIMessage, UIMessageChunk } from 'ai'
import type { ACPAgentDef } from '@/constants'

export { formatConnectionError }

async function currentToolProfile(): Promise<ToolProfileId> {
  try {
    const profile = await window.rev.toolProfile.get()
    if (profile === 'browse' || profile === 'reverse' || profile === 'probe') return profile
  } catch (e) {
    console.warn('[tool-profile] read failed:', e)
  }
  return 'browse'
}

export class ACPChatTransport implements ChatTransport<UIMessage> {
  private sessionId: string | null = null
  private agentDef: ACPAgentDef
  private cwd: string
  private systemPrompt: string
  private sentContext = false
  // A turn the agent started on its own between prompts: updates queue here
  // until the chat attaches via reconnectToStream, then flow to bgSink.
  private bgQueue: Array<Parameters<typeof mapUpdate>[0]> = []
  private bgSink: ((update: Parameters<typeof mapUpdate>[0]) => void) | null = null
  private bgListener: (() => void) | null = null

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
    this.bgQueue = []
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
      const { sessionId } = await window.rev.acp.spawn(this.agentDef.id, this.cwd)
      this.sessionId = sessionId
    }

    const alreadySentContext = this.sentContext
    const body = alreadySentContext ? text : `${this.systemPrompt}\n\n${text}`
    // Every turn, including the first. The profile can change between messages.
    const promptText = `${body}\n${toolProfileTurnLine(await currentToolProfile())}`
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
        let userStopped = false
        // Full agent text for this turn, so the safeguard/fallback notice can be
        // detected on the complete message at finish — it is split across
        // streamed chunk boundaries, so per-delta matching is unreliable.
        let agentText = ''
        const seenToolCallIds = new Set<string>()

        // Watchdog: if 60s pass with NO notification at all, surface as error
        // so the chat status doesn't get stuck on "streaming" forever.
        // A live turn that is only quiet (model thinking, one long tool) emits
        // rever_pulse every 15s from the main process, which rearms this timer.
        let watchdog: ReturnType<typeof setTimeout> | null = null
        const armWatchdog = () => {
          if (watchdog) clearTimeout(watchdog)
          watchdog = setTimeout(() => {
            if (closed) return
            // Waiting on the user's approval is not the agent going silent.
            if (hasPendingPermission(sessionId)) {
              armWatchdog()
              return
            }
            cancelPendingPermissions(sessionId)
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
          errorText?: string,
          opts?: { refusal?: boolean; safeguard?: boolean }
        ) => {
          if (closed) return
          closed = true
          clearWatchdog()
          if (errorText) controller.enqueue({ type: 'error', errorText })
          if (textStarted) controller.enqueue({ type: 'text-end', id: textId })
          // A provider safety event: render a distinct banner (a `data-notice`
          // part) instead of letting it blend into the answer or show the
          // misleading "agent returned no response" empty-turn note.
          //   refusal  — hard block (ACP stopReason 'refusal'); nothing ran.
          //   safeguard — soft flag + model fallback; the answer still came back
          //               but the notice arrived as plain text (see
          //               detectSafeguardNotice).
          const notice = opts?.refusal
            ? { kind: 'refusal', title: t('chat.refusedTitle'), text: t('chat.refused') }
            : opts?.safeguard
              ? { kind: 'safeguard', title: t('chat.safeguardTitle'), text: t('chat.safeguard') }
              : null
          if (notice) {
            controller.enqueue({ type: 'data-notice', data: notice } as unknown as UIMessageChunk)
          }
          // Surface empty-turn case so users don't think the agent ignored them.
          // A user Stop isn't an empty answer — don't blame the agent for it.
          else if (!errorText && !anyContent && !userStopped) {
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
          userStopped = true
          cancelPendingPermissions(sessionId)
          void window.rev.acp.cancel(sessionId)
          finish('stop')
          // session/cancel is only a notification — some agents keep running
          // (e.g. mid MCP tool call). If the prompt doesn't settle within 5s,
          // kill the child so it can't keep driving the browser; the next
          // message spawns a fresh session.
          const killTimer = setTimeout(() => {
            void window.rev.acp.kill(sessionId).catch(() => null)
            if (self.sessionId === sessionId) {
              self.sessionId = null
              self.sentContext = false
            }
          }, 5_000)
          promptPromise.then(
            () => clearTimeout(killTimer),
            () => clearTimeout(killTimer)
          )
        })

        controller.enqueue({ type: 'start' })
        controller.enqueue({ type: 'start-step' })
        armWatchdog()

        const promptPromise = window.rev.acp
          .prompt(sessionId, promptText, (notification) => {
            if (closed) return
            armWatchdog()
            const update = (notification as unknown as { update: Parameters<typeof mapUpdate>[0] }).update
            if (
              update.sessionUpdate === 'agent_message_chunk' &&
              update.content?.type === 'text' &&
              typeof update.content.text === 'string'
            ) {
              agentText += update.content.text
            }
            const result = mapUpdate(update, textId, textStarted, { seenToolCallIds })
            if (result.chunks.length > 0) anyContent = true
            for (const chunk of result.chunks) controller.enqueue(chunk)
            textStarted = result.textStarted
          })
          .then((res) => {
            // 프롬프트가 성공적으로 완료된 후에 sentContext를 true로 설정
            // (실패 시에는 시스템 프롬프트가 다음 시도에서 재전송됨)
            if (!alreadySentContext) self.sentContext = true
            if (res.stopReason === 'refusal') {
              finish('other', undefined, { refusal: true })
              return
            }
            const reason = res.stopReason === 'end_turn' ? 'stop' : 'other'
            // Soft safeguard + model fallback: the turn completed, but the SDK
            // streamed its notice as plain text. Flag it with a banner too.
            if (detectSafeguardNotice(agentText)) {
              finish(reason, undefined, { safeguard: true })
              return
            }
            finish(reason)
          })
          .catch((e) => {
            finish('error', formatConnectionError(e))
          })
      }
    })
  }

  /**
   * Subscribe to turns the agent runs on its own (e.g. once a background task
   * finishes). `onActivity` fires when one starts; the chat should then call
   * useChat's resumeStream(), which attaches through reconnectToStream.
   */
  watchBackground(onActivity: () => void): () => void {
    // A renderer hot-reloaded over a preload built before this API existed.
    if (typeof window.rev.acp.onIdleUpdate !== 'function') return () => {}
    this.bgListener = onActivity
    const off = window.rev.acp.onIdleUpdate((sessionId, notification) => {
      if (sessionId !== this.sessionId) return
      const update = (notification as unknown as { update: Parameters<typeof mapUpdate>[0] }).update
      if (this.bgSink) {
        this.bgSink(update)
        return
      }
      if (this.bgQueue.length === 0 && !startsBackgroundTurn(update)) return
      this.bgQueue.push(update)
      if (this.bgQueue.length === 1) this.bgListener?.()
    })
    return () => {
      off()
      if (this.bgListener === onActivity) this.bgListener = null
    }
  }

  hasPendingBackground(): boolean {
    return this.bgQueue.length > 0 && !this.bgSink
  }

  async reconnectToStream(): Promise<ReadableStream<UIMessageChunk> | null> {
    const sessionId = this.sessionId
    if (!sessionId || this.bgQueue.length === 0 || this.bgSink) return null
    const queued = this.bgQueue
    this.bgQueue = []
    // eslint-disable-next-line @typescript-eslint/no-this-alias
    const self = this
    let closeStream: (() => void) | null = null

    return new ReadableStream<UIMessageChunk>({
      start: (controller) => {
        const textId = `text-${Date.now()}-bg`
        let textStarted = false
        let closed = false
        // Only this turn's tool calls: a late status refresh of an earlier
        // turn's call has no part in the message being continued.
        const seenToolCallIds = new Set<string>()

        // The result usage_update normally ends the turn; this only guards a
        // turn whose result never arrives.
        let quietTimer: ReturnType<typeof setTimeout> | null = null
        const armQuietTimer = () => {
          if (quietTimer) clearTimeout(quietTimer)
          quietTimer = setTimeout(() => finish(), 120_000)
        }

        const finish = () => {
          if (closed) return
          closed = true
          if (quietTimer) clearTimeout(quietTimer)
          if (self.bgSink === push) self.bgSink = null
          if (textStarted) controller.enqueue({ type: 'text-end', id: textId })
          controller.enqueue({ type: 'finish-step' })
          controller.enqueue({ type: 'finish', finishReason: 'stop' })
          controller.close()
        }
        closeStream = () => {
          closed = true
          if (quietTimer) clearTimeout(quietTimer)
          if (self.bgSink === push) self.bgSink = null
        }

        const push = (update: Parameters<typeof mapUpdate>[0]) => {
          if (closed) return
          armQuietTimer()
          if (endsBackgroundTurn(update)) {
            finish()
            return
          }
          if (update.sessionUpdate === 'tool_call_update' && !seenToolCallIds.has(update.toolCallId)) return
          const result = mapUpdate(update, textId, textStarted, { seenToolCallIds })
          for (const chunk of result.chunks) controller.enqueue(chunk)
          textStarted = result.textStarted
        }

        controller.enqueue({ type: 'start' })
        controller.enqueue({ type: 'start-step' })
        self.bgSink = push
        armQuietTimer()
        for (const update of queued) push(update)
      },
      cancel: () => {
        // The user hit Stop on the background turn.
        closeStream?.()
        void window.rev.acp.cancel(sessionId).catch(() => null)
      }
    })
  }

  async destroy(): Promise<void> {
    if (this.sessionId) {
      await window.rev.acp.kill(this.sessionId).catch(() => null)
      this.sessionId = null
    }
  }

}
