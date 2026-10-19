// Serves the macro MCP tools (main/mcp/tools/macros.ts). Saved workflows live
// in a renderer zustand store, so main asks us over the preload bridge.
import { attachExtract, parseExtract, type CliData } from '../../../../shared/cli-spec'
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

function cliWorkflows(): Workflow[] {
  return useWorkflowsStore.getState().workflows.filter((w) => w.kind === 'cli')
}

function findCli(id: string): Workflow {
  const w = cliWorkflows().find((item) => item.id === id)
  if (!w) throw new Error(`No CLI with id "${id}"`)
  return w
}

function asCli(data: unknown): CliData {
  const row = (data ?? {}) as Partial<CliData>
  return {
    site: typeof row.site === 'string' ? row.site : '',
    title: typeof row.title === 'string' ? row.title : '',
    baseUrl: typeof row.baseUrl === 'string' ? row.baseUrl : '',
    commands: Array.isArray(row.commands) ? row.commands.map((command) => attachExtract(command)) : [],
    writes: row.writes === true
  }
}

function cliSummary(w: Workflow) {
  const data = asCli(w.data)
  return {
    id: w.id,
    name: w.name,
    site: data.site,
    baseUrl: data.baseUrl,
    writes: data.writes,
    commands: data.commands.map((command) => ({
      name: command.name,
      method: command.method,
      path: command.path,
      effect: command.effect,
      ...(command.extract
        ? { extract: command.extract.fields.map((field) => field.name).join(', ') }
        : {})
    }))
  }
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

    case 'cli:list':
      return cliWorkflows().map(cliSummary)

    case 'cli:get': {
      const w = findCli(String(p.id))
      return { id: w.id, name: w.name, data: asCli(w.data) }
    }

    case 'cli:save': {
      const incoming = asCli(p.data)
      if (incoming.commands.length === 0) throw new Error('CLI has no commands')
      const now = Date.now()
      const id = typeof p.id === 'string' && p.id ? p.id : newWorkflowId()
      const existing = cliWorkflows().find((item) => item.id === id)
      const w: Workflow = {
        id,
        kind: 'cli',
        name: String(p.name ?? existing?.name ?? incoming.site ?? 'CLI'),
        data: { ...incoming, writes: false } satisfies CliData,
        createdAt: existing?.createdAt ?? now,
        updatedAt: now
      }
      store.upsert(w)
      return cliSummary(w)
    }

    case 'cli:set-writes': {
      const w = findCli(String(p.id))
      const next: Workflow = { ...w, data: { ...asCli(w.data), writes: p.writes === true } satisfies CliData }
      store.upsert(next)
      return cliSummary(next)
    }

    case 'cli:set-extract': {
      const w = findCli(String(p.id))
      const data = asCli(w.data)
      const name = String(p.command ?? '')
      const parsed = parseExtract(p.extract)
      if (!parsed.ok || !parsed.extract) throw new Error(parsed.ok ? 'extract is required' : parsed.error)
      if (!data.commands.some((item) => item.name === name)) throw new Error(`unknown command: ${name}`)
      const commands = data.commands.map((item) =>
        item.name === name ? { ...item, extract: parsed.extract } : item
      )
      const next: Workflow = { ...w, data: { ...data, commands } satisfies CliData }
      store.upsert(next)
      return cliSummary(next)
    }

    case 'cli:effect': {
      const data = asCli(findCli(String(p.id)).data)
      const command = data.commands.find((item) => item.name === String(p.command))
      if (!command) return 'unknown'
      if (command.effect === 'read' || command.effect === 'write' || command.effect === 'unknown') {
        return command.effect
      }
      return 'unknown'
    }

    default:
      throw new Error(`Unknown bridge op "${op}"`)
  }
}
