import { killSession, promptSession, spawnSession } from './agent-router'
import { callMcpTool } from './mcp/bridge'
import { evalInPage } from './mcp/cdp-eval'
import { hasApiKey } from './settings'

import type { SessionNotification } from '@agentclientprotocol/sdk'

// Deterministic workflow runner: executes a fixed list of MCP tool calls in
// order, with no model in the loop. Tools act on the active tab's CDP target
// (same single-active-target model the agent's browser tools use). Variable
// substitution ({{var}}) is resolved by the renderer before steps arrive here,
// so this stays a plain sequencer.

export interface RunStep {
  tool: string
  input: Record<string, unknown>
  // Preconditions, both applied BEFORE the tool runs: wait until `waitFor`
  // matches an element, then pause a further `delay` ms. Pages that render
  // asynchronously need the selector wait; `delay` covers the cases where
  // there's nothing specific to wait on (animations, debounced handlers).
  waitFor?: string
  delay?: number
  waitTimeout?: number
  // Store this step's result in the run's variable bag under this name, so
  // later steps can interpolate it as {{name}} (or {{name.path.to.field}}).
  saveAs?: string
}

export interface StepProgress {
  index: number
  tool: string
  status: 'waiting' | 'running' | 'done' | 'error'
  output?: string
  error?: string
  waitingFor?: string
}

// Only one workflow runs at a time; a second run supersedes the first.
let activeRun: { cancelled: boolean } | null = null

export function cancelWorkflow(): void {
  if (activeRun) activeRun.cancelled = true
}

const DEFAULT_WAIT_TIMEOUT_MS = 10_000
const POLL_MS = 100

// Sleep in POLL_MS slices so Stop takes effect during a long wait instead of
// after it.
async function sleepCancellable(ms: number, run: { cancelled: boolean }): Promise<void> {
  const until = Date.now() + ms
  while (Date.now() < until) {
    if (run.cancelled) return
    await new Promise((r) => setTimeout(r, Math.min(POLL_MS, until - Date.now())))
  }
}

/**
 * Apply a step's preconditions. Throws if `waitFor` never matches within the
 * timeout — the caller turns that into a normal step error, so a macro fails
 * loudly on a selector that never appears instead of acting on a half-built
 * page.
 */
async function applyStepWaits(
  step: RunStep,
  run: { cancelled: boolean },
  onWaiting: (reason: string) => void
): Promise<void> {
  if (step.waitFor) {
    const selector = step.waitFor
    const timeout = step.waitTimeout ?? DEFAULT_WAIT_TIMEOUT_MS
    onWaiting(`waiting for ${selector}`)
    const deadline = Date.now() + timeout
    for (;;) {
      if (run.cancelled) return
      let found = false
      try {
        found = await evalInPage<boolean>(
          `!!document.querySelector(${JSON.stringify(selector)})`
        )
      } catch {
        // No active target yet, or the page is mid-navigation. Keep polling
        // until the deadline rather than failing on a transient error.
        found = false
      }
      if (found) break
      if (Date.now() >= deadline) {
        throw new Error(`waitFor timed out after ${timeout}ms: no element matches ${selector}`)
      }
      await new Promise((r) => setTimeout(r, POLL_MS))
    }
  }
  if (step.delay && step.delay > 0) {
    onWaiting(`waiting ${step.delay}ms`)
    await sleepCancellable(step.delay, run)
  }
}

// ── Runtime variables ───────────────────────────────────────────────────────
// The renderer substitutes user-supplied {{vars}} before steps arrive, but
// leaves unknown placeholders intact — those are filled here from step results
// captured via `saveAs`, which is what makes an AI step's output usable
// downstream.

function resolvePath(vars: Record<string, unknown>, path: string): unknown {
  let cur: unknown = vars
  for (const key of path.split('.')) {
    if (cur == null || typeof cur !== 'object') return undefined
    cur = (cur as Record<string, unknown>)[key]
  }
  return cur
}

const PLACEHOLDER = /\{\{\s*([\w.-]+)\s*\}\}/g

function stringifyVar(v: unknown): string {
  return typeof v === 'string' ? v : JSON.stringify(v)
}

export function substitute(value: unknown, vars: Record<string, unknown>): unknown {
  if (typeof value === 'string') {
    // A string that is exactly one placeholder resolves to the raw value, so
    // {"limit": "{{n}}"} yields a number rather than the string "5".
    const whole = value.match(/^\{\{\s*([\w.-]+)\s*\}\}$/)
    if (whole) {
      const v = resolvePath(vars, whole[1])
      return v === undefined ? value : v
    }
    return value.replace(PLACEHOLDER, (m, path: string) => {
      const v = resolvePath(vars, path)
      // Leave unknown placeholders visible instead of blanking them — a typo
      // should surface in the tool error, not silently become "".
      return v === undefined ? m : stringifyVar(v)
    })
  }
  if (Array.isArray(value)) return value.map((v) => substitute(v, vars))
  if (value && typeof value === 'object') {
    const out: Record<string, unknown> = {}
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      out[k] = substitute(v, vars)
    }
    return out
  }
  return value
}

// ── AI step ─────────────────────────────────────────────────────────────────
// A macro step can call the agent instead of an MCP tool. The session is
// spawned on first use and torn down when the run ends, so a macro never
// disturbs (or depends on) the chat panel's conversation.

export const AI_TOOL = '__ai_prompt'

interface AiStepInput {
  prompt?: string
  /** Force a JSON-only answer and parse it before storing. */
  json?: boolean
  /** Shape hint appended to the prompt, e.g. '{ "title": string }'. */
  schema?: string
  /** Override the agent: 'anthropic' | 'openai' | 'grok' | an ACP binary id. */
  agent?: string
}

interface AiSession {
  id: string
  agentId: string
}

function pickAgentId(requested?: string): string {
  if (requested) return requested
  // Prefer a direct API provider when a key is configured — no child process
  // to spawn, so the first AI step doesn't pay several seconds of startup.
  if (hasApiKey('anthropic')) return 'anthropic'
  if (hasApiKey('openai')) return 'openai'
  if (hasApiKey('grok')) return 'grok'
  return 'claude-code'
}

function agentDefFor(agentId: string): { id: string; command: string; args: string[] } {
  if (agentId === 'anthropic' || agentId === 'openai' || agentId === 'grok') {
    return { id: agentId, command: '', args: [] }
  }
  if (agentId === 'codex') return { id: 'codex', command: 'codex-acp', args: [] }
  return { id: 'claude-code', command: 'claude-agent-acp', args: [] }
}

export function stripFences(text: string): string {
  const fenced = text.match(/```(?:json)?\s*([\s\S]*?)```/i)
  return (fenced ? fenced[1] : text).trim()
}

async function runAiStep(
  raw: AiStepInput,
  ctx: { session: AiSession | null; cwd: string }
): Promise<{ session: AiSession; text: string; value: unknown }> {
  const prompt = (raw.prompt ?? '').trim()
  if (!prompt) throw new Error(`${AI_TOOL} requires a "prompt"`)

  const agentId = pickAgentId(raw.agent)
  let session = ctx.session
  if (!session || session.agentId !== agentId) {
    if (session) await killSession(session.id).catch(() => null)
    const { sessionId } = await spawnSession(agentDefFor(agentId), ctx.cwd)
    session = { id: sessionId, agentId }
  }

  let full = prompt
  if (raw.json) {
    full += raw.schema
      ? `\n\nRespond with ONLY valid JSON matching this shape, no markdown fences and no prose:\n${raw.schema}`
      : '\n\nRespond with ONLY valid JSON. No markdown fences, no prose, no explanation.'
  }

  let text = ''
  const onUpdate = (n: SessionNotification): void => {
    const u = (n as unknown as { update?: { sessionUpdate?: string; content?: { type?: string; text?: string } } }).update
    if (u?.sessionUpdate === 'agent_message_chunk' && u.content?.type === 'text') {
      text += u.content.text ?? ''
    }
  }
  await promptSession(session.id, full, onUpdate)
  text = text.trim()
  if (!text) throw new Error('agent returned no text')

  if (!raw.json) return { session, text, value: text }
  const cleaned = stripFences(text)
  try {
    return { session, text: cleaned, value: JSON.parse(cleaned) }
  } catch {
    throw new Error(`agent did not return valid JSON. Got: ${cleaned.slice(0, 200)}`)
  }
}

export async function runWorkflow(
  steps: RunStep[],
  onProgress: (p: StepProgress) => void,
  opts: { cwd?: string } = {}
): Promise<StepProgress[]> {
  // Supersede any in-flight run.
  if (activeRun) activeRun.cancelled = true
  const run = { cancelled: false }
  activeRun = run

  const results: StepProgress[] = []
  const vars: Record<string, unknown> = {}
  let aiSession: AiSession | null = null
  try {
    for (let i = 0; i < steps.length; i++) {
      if (run.cancelled) break
      const step = steps[i]
      try {
        await applyStepWaits(step, run, (reason) =>
          onProgress({ index: i, tool: step.tool, status: 'waiting', waitingFor: reason })
        )
        if (run.cancelled) break
        onProgress({ index: i, tool: step.tool, status: 'running' })

        // Fill {{placeholders}} left over from the renderer pass with values
        // captured by earlier steps' saveAs.
        const input = substitute(step.input, vars) as Record<string, unknown>

        let text: string
        let isError = false
        let saved: unknown
        if (step.tool === AI_TOOL) {
          const res = await runAiStep(input as AiStepInput, {
            session: aiSession,
            cwd: opts.cwd ?? process.cwd()
          })
          aiSession = res.session
          text = res.text
          saved = res.value
        } else {
          const res = await callMcpTool(step.tool, input)
          text = res.text
          isError = res.isError
          saved = res.text
        }
        if (!isError && step.saveAs) vars[step.saveAs] = saved

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
    if (aiSession) await killSession(aiSession.id).catch(() => null)
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
