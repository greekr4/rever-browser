import { TOOL_BUCKETS } from '../mcp/tool-profile'

// OpenAI's Chat Completions API rejects requests carrying more than 128 tools
// (Anthropic has no such limit). Profile filtering runs first. This cap is the
// backstop after that filter: drop the nine bp_* names (hidden from every
// in-app profile), then non-core names from the tail. Core stays even if the
// list is still over the cap. The caller logs `console.warn`, not `console.error`.

export const OPENAI_MAX_TOOLS = 128

// Ordered "drop first" list. Only as many entries as needed are removed.
export const OPENAI_DROP_ORDER: string[] = [
  // Native dialogs made the page-side override re-inject a rarely needed fallback.
  'dialog_inject_override',
  'dialog_clear_history',
  'bp_add',
  'bp_list',
  'bp_remove',
  'bp_status',
  'bp_resume',
  'bp_step_over',
  'bp_step_into',
  'bp_step_out',
  'bp_eval_in_frame'
]

export interface CapResult<T> {
  tools: T[]
  dropped: string[]
}

// Trim `tools` to at most OPENAI_MAX_TOOLS, removing OPENAI_DROP_ORDER entries
// first (in order) and, only if that isn't enough, non-core names from the tail.
export function capToolsForOpenAI<T extends { function: { name: string } }>(
  tools: T[]
): CapResult<T> {
  if (tools.length <= OPENAI_MAX_TOOLS) return { tools, dropped: [] }

  const drop = new Set<string>()
  const present = new Set(tools.map((t) => t.function.name))
  for (const name of OPENAI_DROP_ORDER) {
    if (tools.length - drop.size <= OPENAI_MAX_TOOLS) break
    if (present.has(name)) drop.add(name)
  }

  let kept = tools.filter((t) => !drop.has(t.function.name))
  if (kept.length > OPENAI_MAX_TOOLS) {
    // Denylist exhausted. Drop non-core names from the end until the list fits.
    // A core tool registered last is skipped, not cut.
    const dropAt = new Set<number>()
    let overflow = kept.length - OPENAI_MAX_TOOLS
    for (let i = kept.length - 1; i >= 0 && overflow > 0; i--) {
      if (TOOL_BUCKETS[kept[i].function.name] === 'core') continue
      dropAt.add(i)
      overflow -= 1
    }
    const next: T[] = []
    for (let i = 0; i < kept.length; i++) {
      if (!dropAt.has(i)) {
        next.push(kept[i])
        continue
      }
      drop.add(kept[i].function.name)
    }
    kept = next
  }

  return { tools: kept, dropped: [...drop] }
}
