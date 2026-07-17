import type { PipeCond, PipeNode } from './types'

// Runnable form (mirrors preload/main ResolvedPipeNode): tool inputs parsed to
// objects with {{var}} substituted.
export type ResolvedPipeNode =
  | { id: string; type: 'tool'; tool: string; input: Record<string, unknown> }
  | { id: string; type: 'if'; cond: PipeCond; then: ResolvedPipeNode[]; else: ResolvedPipeNode[] }

export type ResolveResult =
  | { ok: true; nodes: ResolvedPipeNode[] }
  | { ok: false; error: string }

function substitute(text: string, vars: Record<string, unknown>): string {
  return text.replace(/\{\{\s*([\w.-]+)\s*\}\}/g, (_m, key: string) => {
    const v = vars[key]
    return v == null ? '' : String(v)
  })
}

function resolveList(nodes: PipeNode[], vars: Record<string, unknown>): ResolvedPipeNode[] {
  return nodes.map((n) => {
    if (n.type === 'tool') {
      if (!n.tool) throw new Error('A step has no tool selected')
      const substituted = substitute(n.input, vars)
      let input: Record<string, unknown> = {}
      if (substituted.trim()) {
        try {
          input = JSON.parse(substituted)
        } catch {
          throw new Error(`Step "${n.tool}" input is not valid JSON`)
        }
      }
      return { id: n.id, type: 'tool', tool: n.tool, input }
    }
    return {
      id: n.id,
      type: 'if',
      cond: n.cond,
      then: resolveList(n.then, vars),
      else: resolveList(n.else, vars)
    }
  })
}

export function resolvePipeline(nodes: PipeNode[], varsText: string): ResolveResult {
  let vars: Record<string, unknown> = {}
  if (varsText.trim()) {
    try {
      vars = JSON.parse(varsText)
    } catch {
      return { ok: false, error: 'Variables are not valid JSON' }
    }
  }
  try {
    return { ok: true, nodes: resolveList(nodes, vars) }
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) }
  }
}
