import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getActiveTarget } from '../../chrome-cdp'
import { scriptUrl } from '../../script-registry'
import { listRequests } from '../../traffic-store'
import { executedFunctions, isTraceableScript, summarizeTrace, type ScriptCoverage, type ScriptInfo } from '../exec-trace'
import { ok, okBudgeted, err, errorMessage } from '../utils'

type Target = NonNullable<ReturnType<typeof getActiveTarget>>

interface Trace {
  target: Target
  startedAt: number
  timer: ReturnType<typeof setTimeout>
  limit: number
  urlFilter?: string
  includeAnonymous: boolean
}

// One trace per tab (webContents id). A finished auto-stop parks its result
// until the next `stop` so it isn't lost.
const traces = new Map<number, Trace>()
const autoStopped = new Map<number, string>()

async function finish(wcId: number, auto: boolean): Promise<string> {
  const trace = traces.get(wcId)
  if (!trace) throw new Error('no exec_trace running on this tab')
  traces.delete(wcId)
  clearTimeout(trace.timer)
  const { target } = trace
  const send = (method: string, params?: object) => target.dbg.sendCommand(method, params)
  const stoppedAt = Date.now()

  let coverage: ScriptCoverage[]
  try {
    const res = (await send('Profiler.takePreciseCoverage')) as { result: ScriptCoverage[] }
    coverage = res.result
  } finally {
    await send('Profiler.stopPreciseCoverage').catch(() => null)
    await send('Profiler.disable').catch(() => null)
  }

  const urlOf = (c: ScriptCoverage) => scriptUrl(target.wc.id, undefined, c.scriptId) ?? c.url
  coverage = coverage.filter(
    (c) =>
      isTraceableScript(urlOf(c), trace.includeAnonymous) &&
      (!trace.urlFilter || urlOf(c).includes(trace.urlFilter))
  )

  // Source text only for scripts that actually ran something — needed to turn
  // offsets into line:col and to place request call sites inside functions.
  const scriptBodies = listRequests({ methodOrType: 'Script' })
  const scripts = new Map<string, ScriptInfo>()
  for (const c of coverage) {
    if (executedFunctions(c).length === 0) continue
    const url = scriptUrl(target.wc.id, undefined, c.scriptId) ?? c.url
    const src = (await send('Debugger.getScriptSource', { scriptId: c.scriptId }).catch(() => null)) as {
      scriptSource?: string
    } | null
    scripts.set(c.scriptId, {
      url,
      source: src?.scriptSource ?? null,
      requestId: url ? (scriptBodies.find((r) => r.url === url)?.requestId ?? null) : null
    })
  }

  const requests = listRequests({ since: trace.startedAt })
    .filter((r) => r.startedAt <= stoppedAt && r.resourceType !== 'Script')
    .map((r) => ({
      requestId: r.requestId,
      method: r.method,
      url: r.url,
      status: r.status,
      initiatorStack: r.initiatorStack
    }))

  const summary = summarizeTrace({ coverage, scripts, requests, limit: trace.limit })
  return JSON.stringify(
    {
      windowMs: stoppedAt - trace.startedAt,
      ...(auto ? { note: 'auto-stopped at the time limit' } : {}),
      ...summary
    },
    null,
    2
  )
}

export function registerExecTraceTools(mcp: McpServer) {
  mcp.registerTool(
    'exec_trace',
    {
      description:
        'Record which JS functions run during an action, via V8 precise coverage. action=start → perform the action (browser_click, or ask the user to click) → action=stop returns every function called in that window (url:line:col, call count), the requests made, and onRequestStack marking functions a request\'s initiator stack passed through — the fastest way to find request-signing code in a large bundle. Feed a function\'s resolveSource to resolve_source. The page runs slower while recording; always stop (auto-stops after maxSeconds).',
      inputSchema: {
        action: z.enum(['start', 'stop', 'status']),
        maxSeconds: z.number().int().positive().max(600).optional().describe('start: auto-stop after this many seconds (default 120)'),
        limit: z.number().int().positive().max(2_000).optional().describe('start: max functions returned (default 150)'),
        urlFilter: z.string().optional().describe('start: only scripts whose URL contains this'),
        includeAnonymous: z
          .boolean()
          .optional()
          .describe('start: also include URL-less scripts (eval / new Function) — default false, they are mostly rever\'s own helpers')
      }
    },
    async ({ action, maxSeconds = 120, limit = 150, urlFilter, includeAnonymous = false }) => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      const wcId = target.wc.id

      if (action === 'status') {
        const t = traces.get(wcId)
        return ok(
          JSON.stringify({
            running: !!t,
            elapsedMs: t ? Date.now() - t.startedAt : null,
            autoStoppedResultWaiting: autoStopped.has(wcId)
          })
        )
      }

      if (action === 'stop') {
        const parked = autoStopped.get(wcId)
        if (parked) {
          autoStopped.delete(wcId)
          return okBudgeted(parked)
        }
        if (!traces.has(wcId)) return err('no exec_trace running — call exec_trace action=start first')
        try {
          return okBudgeted(await finish(wcId, false))
        } catch (e) {
          return err(errorMessage(e))
        }
      }

      if (traces.has(wcId)) return err('exec_trace already running on this tab — call action=stop first')
      autoStopped.delete(wcId)
      try {
        await target.dbg.sendCommand('Profiler.enable')
        await target.dbg.sendCommand('Profiler.startPreciseCoverage', { callCount: true, detailed: true })
        // Resetting sample: counters now start from zero, so the stop sample
        // covers only the window.
        await target.dbg.sendCommand('Profiler.takePreciseCoverage')
      } catch (e) {
        await target.dbg.sendCommand('Profiler.stopPreciseCoverage').catch(() => null)
        return err(errorMessage(e))
      }
      const timer = setTimeout(() => {
        finish(wcId, true)
          .then((r) => autoStopped.set(wcId, r))
          .catch(() => null)
      }, maxSeconds * 1000)
      traces.set(wcId, { target, startedAt: Date.now(), timer, limit, urlFilter, includeAnonymous })
      return ok(
        JSON.stringify({
          started: true,
          autoStopSeconds: maxSeconds,
          next: 'Perform the action now (browser_click, or ask the user to click), then call exec_trace action=stop.'
        })
      )
    }
  )
}
