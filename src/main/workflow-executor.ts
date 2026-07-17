import { callMcpTool } from './mcp/bridge'

// Deterministic workflow runner: executes a fixed list of MCP tool calls in
// order, with no model in the loop. Tools act on the active tab's CDP target
// (same single-active-target model the agent's browser tools use). Variable
// substitution ({{var}}) is resolved by the renderer before steps arrive here,
// so this stays a plain sequencer.

export interface RunStep {
  tool: string
  input: Record<string, unknown>
}

export interface StepProgress {
  index: number
  tool: string
  status: 'running' | 'done' | 'error'
  output?: string
  error?: string
}

// Only one workflow runs at a time; a second run supersedes the first.
let activeRun: { cancelled: boolean } | null = null

export function cancelWorkflow(): void {
  if (activeRun) activeRun.cancelled = true
}

export async function runWorkflow(
  steps: RunStep[],
  onProgress: (p: StepProgress) => void
): Promise<StepProgress[]> {
  // Supersede any in-flight run.
  if (activeRun) activeRun.cancelled = true
  const run = { cancelled: false }
  activeRun = run

  const results: StepProgress[] = []
  try {
    for (let i = 0; i < steps.length; i++) {
      if (run.cancelled) break
      const step = steps[i]
      onProgress({ index: i, tool: step.tool, status: 'running' })
      try {
        const { text, isError } = await callMcpTool(step.tool, step.input)
        const result: StepProgress = isError
          ? { index: i, tool: step.tool, status: 'error', error: text }
          : { index: i, tool: step.tool, status: 'done', output: text }
        onProgress(result)
        results.push(result)
        // Stop the sequence on the first failing step.
        if (isError) break
      } catch (e) {
        const result: StepProgress = {
          index: i,
          tool: step.tool,
          status: 'error',
          error: e instanceof Error ? e.message : String(e)
        }
        onProgress(result)
        results.push(result)
        break
      }
    }
  } finally {
    if (activeRun === run) activeRun = null
  }
  return results
}

// ── Pipeline (branch-aware) execution ───────────────────────────────────────
// The pipeline module compiles its node graph to this resolved tree (inputs
// already parsed + {{var}} substituted) and we walk it, branching on the last
// tool result. Conditions let a pipeline handle errors instead of hard-stopping.

export interface PipeCond {
  on: 'output' | 'error'
  op: 'contains' | 'equals' | 'matches' | 'always'
  value: string
}

export type ResolvedPipeNode =
  | { id: string; type: 'tool'; tool: string; input: Record<string, unknown> }
  | { id: string; type: 'if'; cond: PipeCond; then: ResolvedPipeNode[]; else: ResolvedPipeNode[] }

export interface PipeProgress {
  nodeId: string
  tool?: string
  status: 'running' | 'done' | 'error' | 'branch'
  output?: string
  error?: string
  taken?: 'then' | 'else'
}

function evalCond(cond: PipeCond, last: { text: string; isError: boolean } | null): boolean {
  if (cond.op === 'always') return true
  const target = cond.on === 'error' ? (last?.isError ? last.text : '') : (last?.text ?? '')
  switch (cond.op) {
    case 'contains':
      return target.includes(cond.value)
    case 'equals':
      return target.trim() === cond.value
    case 'matches':
      try {
        return new RegExp(cond.value).test(target)
      } catch {
        return false
      }
    default:
      return false
  }
}

export async function runPipeline(
  nodes: ResolvedPipeNode[],
  onProgress: (p: PipeProgress) => void
): Promise<void> {
  if (activeRun) activeRun.cancelled = true
  const run = { cancelled: false }
  activeRun = run

  let last: { text: string; isError: boolean } | null = null

  const walk = async (list: ResolvedPipeNode[]): Promise<void> => {
    for (const node of list) {
      if (run.cancelled) return
      if (node.type === 'tool') {
        onProgress({ nodeId: node.id, tool: node.tool, status: 'running' })
        try {
          const res = await callMcpTool(node.tool, node.input)
          last = res
          onProgress(
            res.isError
              ? { nodeId: node.id, tool: node.tool, status: 'error', error: res.text }
              : { nodeId: node.id, tool: node.tool, status: 'done', output: res.text }
          )
        } catch (e) {
          last = { text: e instanceof Error ? e.message : String(e), isError: true }
          onProgress({ nodeId: node.id, tool: node.tool, status: 'error', error: last.text })
        }
      } else {
        const taken = evalCond(node.cond, last) ? 'then' : 'else'
        onProgress({ nodeId: node.id, status: 'branch', taken })
        await walk(taken === 'then' ? node.then : node.else)
      }
    }
  }

  try {
    await walk(nodes)
  } finally {
    if (activeRun === run) activeRun = null
  }
}
