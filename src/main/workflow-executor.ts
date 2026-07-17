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
