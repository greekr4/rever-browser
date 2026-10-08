// Pure helpers for exec_trace (electron-free, vitest-testable).
//
// Idea adapted from morluto/rea's observe_web_execution (MIT): V8 precise
// coverage between two points in time tells which functions actually ran
// while the user (or agent) did something — a much smaller haystack than a
// multi-megabyte bundle when hunting for request-signing code.

// The CDP Profiler.ScriptCoverage fields this tool reads.
export interface ScriptCoverage {
  scriptId: string
  url: string
  functions: Array<{
    functionName: string
    isBlockCoverage: boolean
    ranges: Array<{ startOffset: number; endOffset: number; count: number }>
  }>
}

export interface ExecutedFunction {
  name: string
  /** UTF-16 offsets into the script source, end exclusive. */
  start: number
  end: number
  /** Calls during the window (V8 call count of the function's own range). */
  calls: number
  /** False = function-granularity only; branch execution inside is unknown. */
  blockCoverage: boolean
}

export interface TraceRequest {
  requestId: string
  method: string
  url: string
  status?: number
  initiatorStack?: Array<{ functionName: string; url: string; lineNumber: number; columnNumber: number }>
}

export interface ScriptInfo {
  url: string
  source: string | null
  /** traffic-store requestId of the script body, for resolve_source. */
  requestId: string | null
}

export const EXEC_TRACE_LIMITATIONS = [
  'Coverage disables V8 optimisation while recording, so the page runs slower and timing-sensitive code may behave differently.',
  'A function missing from the result is "unknown", not "did not run": already-compiled code may report function-level counts only (blockCoverage=false).',
  'URL-less scripts (eval / new Function, and rever\'s own injected helpers) are hidden unless includeAnonymous=true — turn it on for eval-heavy obfuscated pages.',
  'Main page only — workers, service workers, cross-origin iframes and WebAssembly internals are not covered.',
  'Co-occurrence is not causation: a function that ran during the window did not necessarily produce a given request. onRequestStack is the stronger signal.'
]

// Scripts worth showing. Electron internals, extensions and DevTools never
// belong to the page. URL-less scripts are mostly rever's own injected code
// (cursor HUD, click machinery) — but a page's eval / new Function code is
// URL-less too, so they're opt-in rather than dropped outright.
export function isTraceableScript(url: string, includeAnonymous: boolean): boolean {
  if (!url) return includeAnonymous
  return !/^(node|chrome-extension|devtools|chrome|extensions):/.test(url)
}

export interface LineIndex {
  starts: number[]
}

export function lineIndex(source: string): LineIndex {
  const starts = [0]
  for (let i = 0; i < source.length; i++) if (source.charCodeAt(i) === 10) starts.push(i + 1)
  return { starts }
}

/** 1-based line and column of a UTF-16 offset. */
export function positionAt(idx: LineIndex, offset: number): { line: number; column: number } {
  let lo = 0
  let hi = idx.starts.length - 1
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1
    if (idx.starts[mid] <= offset) lo = mid
    else hi = mid - 1
  }
  return { line: lo + 1, column: offset - idx.starts[lo] + 1 }
}

/** Offset of a 0-based (line, column), as CDP stack frames report them. */
export function offsetAt(idx: LineIndex, line0: number, column0: number): number | null {
  if (line0 < 0 || line0 >= idx.starts.length) return null
  return idx.starts[line0] + column0
}

export function executedFunctions(script: ScriptCoverage): ExecutedFunction[] {
  const out: ExecutedFunction[] = []
  for (const fn of script.functions) {
    const own = fn.ranges[0]
    if (!own || own.count <= 0) continue
    // The script's top-level body: anonymous and starting at 0. It "runs"
    // only when the script loads, which says nothing about the action.
    if (fn.functionName === '' && own.startOffset === 0) continue
    out.push({
      name: fn.functionName,
      start: own.startOffset,
      end: own.endOffset,
      calls: own.count,
      blockCoverage: fn.isBlockCoverage
    })
  }
  return out.sort((a, b) => a.start - b.start)
}

export function summarizeTrace(input: {
  coverage: ScriptCoverage[]
  scripts: Map<string, ScriptInfo>
  requests: TraceRequest[]
  limit: number
}) {
  let budget = input.limit
  let total = 0
  const scripts = []
  for (const cov of input.coverage) {
    const fns = executedFunctions(cov)
    if (fns.length === 0) continue
    total += fns.length
    const info = input.scripts.get(cov.scriptId)
    const url = info?.url || cov.url || `script#${cov.scriptId}`
    const idx = info?.source ? lineIndex(info.source) : null

    // Offsets of request call sites that sit in this script.
    const callSites: Array<{ requestId: string; offset: number }> = []
    if (idx) {
      for (const r of input.requests) {
        for (const f of r.initiatorStack ?? []) {
          if (f.url !== url) continue
          const off = offsetAt(idx, f.lineNumber, f.columnNumber)
          if (off !== null) callSites.push({ requestId: r.requestId, offset: off })
        }
      }
    }

    const kept = fns.slice(0, Math.max(0, budget))
    budget -= kept.length
    if (kept.length === 0) continue
    scripts.push({
      url,
      functions: kept.map((fn) => {
        const pos = idx ? positionAt(idx, fn.start) : null
        return {
          name: fn.name || '(anonymous)',
          location: pos ? `${url}:${pos.line}:${pos.column}` : `${url}@${fn.start}`,
          calls: fn.calls,
          blockCoverage: fn.blockCoverage,
          onRequestStack: [
            ...new Set(
              callSites.filter((c) => c.offset >= fn.start && c.offset < fn.end).map((c) => c.requestId)
            )
          ],
          resolveSource:
            pos && info?.requestId
              ? { requestId: info.requestId, line: pos.line - 1, column: pos.column - 1 }
              : null
        }
      })
    })
  }
  return {
    totalFunctions: total,
    omittedFunctions: total - (input.limit - Math.max(0, budget)),
    scripts,
    requests: input.requests.map((r) => ({
      requestId: r.requestId,
      method: r.method,
      url: r.url,
      status: r.status
    })),
    limitations: EXEC_TRACE_LIMITATIONS
  }
}
