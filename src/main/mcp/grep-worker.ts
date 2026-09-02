import { Worker } from 'node:worker_threads'

// Running an agent-supplied regex against a multi-megabyte script body on the
// main thread is a denial-of-service risk: one catastrophic-backtracking
// pattern (e.g. /(a+)+$/) blocks the whole browser UI indefinitely, because a
// single RegExp.exec call cannot be interrupted. We run the exec loop in a
// worker thread and terminate it if it blows the wall-clock budget.

export interface RawMatch {
  index: number
  match: string
}

export class GrepTimeoutError extends Error {
  constructor(ms: number) {
    super(`pattern timed out after ${ms}ms (possible catastrophic backtracking)`)
    this.name = 'GrepTimeoutError'
  }
}

// Self-contained worker source (run via eval, so it needs no separate build
// entry in electron-vite). It collects raw {index, match} pairs; the caller
// does the cheap dedup / snippet slicing on the main thread.
const WORKER_SRC = `
const { parentPort, workerData } = require('node:worker_threads')
const { body, source, flags, max } = workerData
const g = flags.includes('g') ? flags : flags + 'g'
const re = new RegExp(source, g)
const out = []
let m
while ((m = re.exec(body)) && out.length < max) {
  if (m[0].length === 0) { re.lastIndex++; continue }
  out.push({ index: m.index, match: m[0] })
}
parentPort.postMessage(out)
`

/**
 * Run `regex` against `body` in a worker with a hard timeout. `max` caps the
 * number of raw matches collected (dedup happens later, so pass a generous
 * multiple of the caller's real limit). Rejects with GrepTimeoutError on
 * overrun.
 */
export function execRegexWithTimeout(
  body: string,
  regex: RegExp,
  max: number,
  timeoutMs = 5000
): Promise<RawMatch[]> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(WORKER_SRC, {
      eval: true,
      workerData: { body, source: regex.source, flags: regex.flags, max }
    })
    let settled = false
    const timer = setTimeout(() => {
      if (settled) return
      settled = true
      void worker.terminate()
      reject(new GrepTimeoutError(timeoutMs))
    }, timeoutMs)

    worker.on('message', (matches: RawMatch[]) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      void worker.terminate()
      resolve(matches)
    })
    worker.on('error', (e) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      reject(e)
    })
  })
}
