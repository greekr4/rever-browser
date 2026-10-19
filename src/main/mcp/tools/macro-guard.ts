import { ALWAYS_RISKY_TOOLS, METHOD_GATED_TOOLS, extractMethod } from '../../../shared/risky-tools'
import type { CatalogKind } from '../tool-profile'

export interface MacroStep {
  tool: string
  input?: Record<string, unknown>
}

const RISKY = new Set<string>(ALWAYS_RISKY_TOOLS)
const METHOD_GATED = new Set<string>(METHOD_GATED_TOOLS)
const SAFE_METHODS = new Set(['GET', 'HEAD'])

export function macroRunNeedsGuard(catalog: CatalogKind): boolean {
  return catalog === 'follow' || catalog === 'direct'
}

export function macroRunRefusal(index: number, tool: string): string {
  return `run_macro refused step ${index} (${tool}): agent-invoked macros cannot run ALWAYS_RISKY_TOOLS, a non-GET replay_request/repeater_send, cli_writes, or a non-read cli_run. Call that tool directly so ACP can prompt, or so an API model can run it once it is advertised. The Workflows panel is unchanged.`
}

export type CliEffectLookup = (id: string, command: string) => Promise<'read' | 'write' | 'unknown'>

async function cliStepForbidden(step: MacroStep, lookup: CliEffectLookup | undefined): Promise<boolean> {
  if (step.tool === 'cli_writes') return true
  if (step.tool !== 'cli_run') return false
  if (!lookup) return true
  const id = step.input?.id
  const command = step.input?.command
  if (typeof id !== 'string' || typeof command !== 'string' || !id || !command) return true
  try {
    return (await lookup(id, command)) !== 'read'
  } catch {
    return true
  }
}

function isForbiddenLeaf(step: MacroStep): boolean {
  if (RISKY.has(step.tool)) return true
  if (!METHOD_GATED.has(step.tool)) return false
  const method = extractMethod(step.input)
  return !method || !SAFE_METHODS.has(method.toUpperCase())
}

export async function findForbiddenMacroStep(
  steps: MacroStep[],
  resolve: (id: string) => Promise<MacroStep[]>,
  stack: readonly string[],
  lookup?: CliEffectLookup
): Promise<{ index: number; tool: string } | null> {
  for (let index = 0; index < steps.length; index++) {
    const step = steps[index]
    if (step.tool === 'run_macro') {
      const id = step.input?.id
      if (typeof id !== 'string' || stack.includes(id)) return { index, tool: 'run_macro' }
      const inner = await resolve(id)
      const hit = await findForbiddenMacroStep(inner, resolve, stack.concat(id), lookup)
      if (hit) return { index, tool: hit.tool }
      continue
    }
    if (isForbiddenLeaf(step)) return { index, tool: step.tool }
    if (await cliStepForbidden(step, lookup)) return { index, tool: step.tool }
  }
  return null
}

/** follow/direct `run_macro` calls this before `runWorkflow`. `ok` is the only path through. */
export async function screenMacroRun(
  steps: MacroStep[],
  resolve: (id: string) => Promise<MacroStep[]>,
  stack: readonly string[],
  lookup?: CliEffectLookup
): Promise<{ ok: true } | { ok: false; error: string }> {
  const hit = await findForbiddenMacroStep(steps, resolve, stack, lookup)
  if (!hit) return { ok: true }
  return { ok: false, error: macroRunRefusal(hit.index, hit.tool) }
}
