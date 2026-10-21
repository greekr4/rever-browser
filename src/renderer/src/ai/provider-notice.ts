// Detects the model provider's safeguard / model-fallback notice that the ACP
// agent streams as plain `agent_message_chunk` text — there is no structured
// stopReason for it (unlike a hard 'refusal'). The transport runs this on the
// turn's full accumulated text at finish (not per-delta: the notice is split
// across chunk boundaries mid-stream) to surface a distinct UI banner instead
// of letting the notice blend into the answer.
//
// Matched against the stable phrasing the Claude Agent SDK emits, e.g.
//   "Fable 5.1's safeguards stopped the response above …"
//   "Model fallback: claude-fable-5-1 declined this request (cyber); retried …"
//   "Opus 4.8's safeguards flagged this message."
// Kept conservative (two-token signatures) so ordinary prose that merely
// mentions "safeguards" or "fallback" does not trip it.
const PATTERNS: RegExp[] = [
  /safeguards?\s+(?:stopped|flagged)\b/i,
  /\bmodel fallback\b[\s\S]{0,120}?\bdeclined this request\b/i,
  /\bdeclined this request\b[\s\S]{0,40}?\(cyber\)/i
]

export function detectSafeguardNotice(text: string | undefined | null): boolean {
  if (!text) return false
  return PATTERNS.some((re) => re.test(text))
}
