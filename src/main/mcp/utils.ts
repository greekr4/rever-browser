export function ok(text: string) {
  return { content: [{ type: 'text' as const, text }] }
}

export function err(text: string) {
  return { isError: true, content: [{ type: 'text' as const, text }] }
}

export function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

// --- Tool-result size budgeting -------------------------------------------
// Large tool results (full request bodies, snapshots, bundle dumps) can eat
// tens of thousands of tokens of agent context in one call. okBudgeted() caps
// what it returns and parks the full text in a small process-global LRU so the
// agent can page the rest with tool_result_more. The store is per-process (not
// per-MCP-session) so a result produced in one session can be paged from
// another (e.g. scripts/mcp-call.py).

const DEFAULT_MAX_CHARS = 12_000
const MAX_STORED_RESULTS = 50

interface StoredResult {
  full: string
}

const resultStore = new Map<string, StoredResult>()
let resultSeq = 0

function putResult(full: string): string {
  const id = `res_${++resultSeq}`
  resultStore.set(id, { full })
  while (resultStore.size > MAX_STORED_RESULTS) {
    const oldest = resultStore.keys().next().value
    if (oldest === undefined) break
    resultStore.delete(oldest)
  }
  return id
}

// Cut `text` to at most `maxChars`, preferring the last newline so a slice
// doesn't end mid-line. Returns the head plus how many chars it kept.
export function budgetText(
  text: string,
  maxChars: number = DEFAULT_MAX_CHARS
): { head: string; truncated: boolean; shownChars: number } {
  if (text.length <= maxChars) return { head: text, truncated: false, shownChars: text.length }
  const hardSlice = text.slice(0, maxChars)
  const lastNl = hardSlice.lastIndexOf('\n')
  // Only snap to a newline if it keeps most of the budget (avoid tiny slices).
  const head = lastNl > maxChars * 0.5 ? hardSlice.slice(0, lastNl) : hardSlice
  return { head, truncated: true, shownChars: head.length }
}

export function okBudgeted(text: string, opts: { maxChars?: number } = {}) {
  const { head, truncated, shownChars } = budgetText(text, opts.maxChars ?? DEFAULT_MAX_CHARS)
  if (!truncated) return ok(text)
  const id = putResult(text)
  const footer =
    `\n\n[truncated: showing ${shownChars} of ${text.length} chars. ` +
    `Call tool_result_more with resultId="${id}", offset=${shownChars} for the rest.]`
  return ok(head + footer)
}

export interface ResultSlice {
  slice: string
  offset: number
  end: number
  total: number
  more: boolean
  nextOffset: number | null
}

export function getResultSlice(
  id: string,
  offset: number,
  limit: number = DEFAULT_MAX_CHARS
): ResultSlice | null {
  const entry = resultStore.get(id)
  if (!entry) return null
  // Refresh LRU recency on access.
  resultStore.delete(id)
  resultStore.set(id, entry)
  const safeOffset = Math.max(0, Math.min(offset, entry.full.length))
  const slice = entry.full.slice(safeOffset, safeOffset + Math.max(1, limit))
  const end = safeOffset + slice.length
  const more = end < entry.full.length
  return { slice, offset: safeOffset, end, total: entry.full.length, more, nextOffset: more ? end : null }
}

// Test-only reset so unit tests don't leak the module-global store.
export function __resetResultStoreForTests() {
  resultStore.clear()
  resultSeq = 0
}
