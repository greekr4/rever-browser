// Serves the macro MCP tools (main/mcp/tools/macros.ts). Saved workflows live
// in a renderer zustand store, so main asks us over the preload bridge.
import { resolveSteps, type MacroData, type MacroStep } from '../macro/MacroEditor'

import { useWorkflowsStore } from './store'
import { newWorkflowId, type Workflow } from './types'

/** Step shape the agent sends/receives: tool + parsed JSON input. */
interface AgentStep {
  tool: string
  input?: Record<string, unknown>
  waitFor?: string
  delay?: number
}

function stepId(): string {
  return `s${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`
}

function toMacroSteps(steps: AgentStep[]): MacroStep[] {
  return steps.map((s) => ({
    id: stepId(),
    tool: s.tool,
    input: JSON.stringify(s.input ?? {}, null, 2),
    ...(s.waitFor ? { waitFor: s.waitFor } : {}),
    ...(s.delay != null ? { delay: s.delay } : {})
  }))
}

function toAgentSteps(steps: MacroStep[]): AgentStep[] {
  return steps.map((s) => {
    let input: Record<string, unknown> = {}
    try {
      if (s.input.trim()) input = JSON.parse(s.input)
    } catch {
      // Keep the raw text visible to the agent so it can fix a broken step.
      return { tool: s.tool, input: { __unparsed: s.input } }
    }
    return {
      tool: s.tool,
      input,
      ...(s.waitFor ? { waitFor: s.waitFor } : {}),
      ...(s.delay != null ? { delay: s.delay } : {})
    }
  })
}

function macros(): Workflow[] {
  return useWorkflowsStore.getState().workflows.filter((w) => w.kind === 'macro')
}

function findMacro(id: string): Workflow {
  const w = macros().find((m) => m.id === id)
  if (!w) throw new Error(`No macro with id "${id}"`)
  return w
}

function summary(w: Workflow) {
  const data = w.data as MacroData
  return {
    id: w.id,
    name: w.name,
    description: w.description ?? '',
    stepCount: data.steps.length,
    updatedAt: w.updatedAt
  }
}

function detail(w: Workflow) {
  const data = w.data as MacroData
  return { ...summary(w), steps: toAgentSteps(data.steps), vars: data.vars }
}

export async function handleAgentRequest(op: string, payload: unknown): Promise<unknown> {
  const p = (payload ?? {}) as Record<string, unknown>
  const store = useWorkflowsStore.getState()

  switch (op) {
    case 'macro:list':
      return macros().map(summary)

    case 'macro:get':
      return detail(findMacro(String(p.id)))

    case 'macro:create': {
      const now = Date.now()
      const w: Workflow = {
        id: newWorkflowId(),
        kind: 'macro',
        name: String(p.name ?? 'Untitled macro'),
        description: p.description ? String(p.description) : undefined,
        data: {
          steps: toMacroSteps((p.steps as AgentStep[]) ?? []),
          vars: p.vars ? String(p.vars) : ''
        } satisfies MacroData,
        createdAt: now,
        updatedAt: now
      }
      store.upsert(w)
      return detail(w)
    }

    case 'macro:update': {
      const w = findMacro(String(p.id))
      const data = w.data as MacroData
      const next: Workflow = {
        ...w,
        name: p.name != null ? String(p.name) : w.name,
        description: p.description != null ? String(p.description) : w.description,
        data: {
          steps: p.steps != null ? toMacroSteps(p.steps as AgentStep[]) : data.steps,
          vars: p.vars != null ? String(p.vars) : data.vars
        } satisfies MacroData
      }
      store.upsert(next)
      return detail(next)
    }

    case 'macro:remove-steps': {
      const w = findMacro(String(p.id))
      const data = w.data as MacroData
      const indexes = new Set((p.indexes as number[]) ?? [])
      const kept = data.steps.filter((_s, i) => !indexes.has(i))
      const next: Workflow = { ...w, data: { ...data, steps: kept } satisfies MacroData }
      store.upsert(next)
      return detail(next)
    }

    // run_macro asks us to resolve rather than resolving in main, so {{var}}
    // substitution has exactly one implementation.
    case 'macro:resolve': {
      const w = findMacro(String(p.id))
      const data = w.data as MacroData
      const resolved = resolveSteps(data.steps, p.vars != null ? String(p.vars) : data.vars)
      if (!resolved.ok) throw new Error(resolved.error)
      if (resolved.steps.length === 0) throw new Error(`Macro "${w.name}" has no steps`)
      return { name: w.name, steps: resolved.steps }
    }

    case 'macro:delete':
      findMacro(String(p.id))
      store.remove(String(p.id))
      return { deleted: true }

    default:
      throw new Error(`Unknown bridge op "${op}"`)
  }
}
