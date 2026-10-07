// Pure helpers for ACP prompt turn handling (electron-free, vitest-testable).

/**
 * claude-agent-acp leaves one stale `session_state_changed: idle` message in
 * the Claude Agent SDK query after a prompt rejects (e.g. an API error). The
 * next prompt consumes it immediately and resolves `end_turn` with zero
 * updates — the user sees an empty turn. Detect that case so the caller can
 * re-issue the prompt once.
 */
export function isStaleEmptyTurn(input: {
  previousPromptFailed: boolean
  updateCount: number
  stopReason: string
  elapsedMs: number
}): boolean {
  if (!input.previousPromptFailed) return false
  if (input.updateCount > 0) return false
  if (input.stopReason !== 'end_turn') return false
  // A real model turn always takes longer than this; the stale one is ~0ms.
  return input.elapsedMs < 1_000
}
