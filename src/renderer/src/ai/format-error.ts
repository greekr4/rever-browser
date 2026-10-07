// Pure error-text formatting for chat turns (no DOM / ?raw imports so vitest
// can cover it). Re-exported by acp-transport.ts.

export function formatConnectionError(e: unknown): string {
  const msg = e instanceof Error ? e.message : String(e)
  if (msg.includes('ENOENT') || msg.includes('spawn')) {
    return `Agent binary not found. Check your PATH. (${msg})`
  }
  if (msg.includes('timeout') || msg.includes('Timeout')) {
    return 'ACP server did not respond in time.'
  }
  if (/rate limit/i.test(msg)) {
    // claude-agent-acp reports an unavailable model (e.g. sonnet[1m] on an
    // account without 1M-context / extra-usage access) as a bare
    // "API Error: Rate limit reached". Say what to do about it.
    return `${msg}. The selected model may not be available on this account (1M-context models need extra-usage access) — switch model or wait and retry.`
  }
  return msg
}
