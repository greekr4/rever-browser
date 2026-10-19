import type { SessionNotification } from '@agentclientprotocol/sdk'

/** Shorter than the renderer's 60s silence watchdog, so a quiet-but-live turn is not cancelled. */
export const TURN_PULSE_MS = 15_000

/**
 * Tell the chat UI this turn is still in progress. Grok can think for minutes
 * before the first token, and Claude Code can sit inside one tool longer than
 * 60s. Those gaps are not a dead agent. Call the returned stop function when
 * the turn ends — including when it throws.
 */
export function startTurnPulse(
  onUpdate: (notification: SessionNotification) => void,
  sessionId: string,
  intervalMs = TURN_PULSE_MS
): () => void {
  const send = () => {
    onUpdate({
      sessionId,
      update: { sessionUpdate: 'rever_pulse' }
    } as unknown as SessionNotification)
  }
  const timer = setInterval(send, intervalMs)
  timer.unref?.()
  return () => clearInterval(timer)
}
