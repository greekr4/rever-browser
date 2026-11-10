export interface WsStep {
  payload: string
  /** Wait this long before sending the frame. */
  delayMs: number
  asBinary?: boolean
}

export type WsSend = (step: WsStep, reconnect: boolean) => Promise<{ ok: boolean; error?: string }>

/**
 * Sends the steps in order and stops at the first one that fails.
 * Only the first step may reopen a closed socket: a server that drops the
 * connection mid-sequence has rejected it, and reconnecting would hide that.
 */
export async function runSequence(
  steps: WsStep[],
  send: WsSend,
  sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms))
): Promise<{ sent: number; error?: string }> {
  for (const [i, step] of steps.entries()) {
    if (step.delayMs > 0) await sleep(step.delayMs)
    const r = await send(step, i === 0)
    if (!r.ok) return { sent: i, error: r.error ?? 'error' }
  }
  return { sent: steps.length }
}
