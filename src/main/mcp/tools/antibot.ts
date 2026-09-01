import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getRequest } from '../../traffic-store'
import { ok, err } from '../utils'

interface Signal {
  name: string
  hit: boolean
  detail: string
  weight: number
}

function analyze(body: string): { signals: Signal[]; score: number } {
  const lines = body.split('\n')
  const maxLine = lines.reduce((m, l) => Math.max(m, l.length), 0)
  const hexIds = (body.match(/_0x[0-9a-f]{4,}/gi) || []).length
  const switchCases = (body.match(/case\s+0?x?[0-9a-f]+\s*:/gi) || []).length
  const bigStringArray = /(?:var|const|let)\s+_0x[0-9a-f]+\s*=\s*\[\s*(['"]).{0,40}\1\s*(?:,\s*(['"]).{0,40}\2\s*){15,}/i.test(
    body
  )
  const rotator = /\(function\s*\(_0x[0-9a-f]+\s*,\s*_0x[0-9a-f]+\)\s*\{[\s\S]{0,400}?while\s*\(/i.test(body)
  const evalFn = (body.match(/\b(eval|Function)\s*\(/g) || []).length
  const singleCharVars = (body.match(/\b(?:var|const|let)\s+[a-z]\b/g) || []).length

  const signals: Signal[] = [
    {
      name: 'single-line-blob',
      hit: maxLine > 5000,
      detail: `longest line = ${maxLine} chars`,
      weight: 1
    },
    {
      name: 'hex-identifiers (_0x…)',
      hit: hexIds > 20,
      detail: `${hexIds} occurrences`,
      weight: 2
    },
    {
      name: 'opcode switch dispatcher',
      hit: switchCases > 15,
      detail: `${switchCases} numeric case labels`,
      weight: 2
    },
    {
      name: 'rotated string-array',
      hit: bigStringArray,
      detail: bigStringArray ? 'large _0x string table found' : 'none',
      weight: 2
    },
    {
      name: 'string-array rotator IIFE',
      hit: rotator,
      detail: rotator ? 'decoder IIFE with while-loop found' : 'none',
      weight: 2
    },
    {
      name: 'eval / Function() usage',
      hit: evalFn > 3,
      detail: `${evalFn} dynamic-eval sites`,
      weight: 1
    },
    {
      name: 'many single-char vars',
      hit: singleCharVars > 40,
      detail: `${singleCharVars} single-char declarations`,
      weight: 1
    }
  ]
  const score = signals.filter((s) => s.hit).reduce((n, s) => n + s.weight, 0)
  return { signals, score }
}

export function registerAntibotTools(mcp: McpServer) {
  mcp.registerTool(
    'detect_antibot_vm',
    {
      description:
        'Heuristically classify a captured script as a custom interpreter / risk-control (antibot) VM — the kind of self-modifying, opcode-dispatched, hex-obfuscated bundle used by Akamai / 风控 / captcha vendors. A high score means static deobfuscation is a dead end and you should plan a runtime approach (crypto_trace, inject a hook, capture the generated token in-browser) instead. Pass a script requestId from list_scripts / list_requests.',
      inputSchema: {
        requestId: z.string().describe('requestId of a script (from list_scripts or list_requests)')
      }
    },
    async ({ requestId }) => {
      const entry = getRequest(requestId)
      if (!entry) return err(`unknown requestId: ${requestId}`)
      if (!entry.responseBody || entry.responseBodyBase64)
        return err(`requestId ${requestId} has no text body to analyze`)
      const { signals, score } = analyze(entry.responseBody)
      const verdict =
        score >= 5 ? 'likely-vm' : score >= 3 ? 'possibly-obfuscated' : 'ordinary'
      const recommendation =
        verdict === 'likely-vm'
          ? 'Do NOT sink time into static deobfuscation. Treat it as a VM: hook the runtime (crypto_trace for signing, inject_add to wrap the token builder) and capture the generated value in-browser. Step-by-step procedure: docs/jsvmp-recovery.md.'
          : verdict === 'possibly-obfuscated'
            ? 'Some obfuscation present; try deobfuscate_script, but be ready to fall back to a runtime hook (see docs/jsvmp-recovery.md).'
            : 'Reads as ordinary/minified code — static analysis (grep_scripts, sourcemap, deobfuscate_script) is fine.'
      const playbook = verdict === 'ordinary' ? undefined : 'docs/jsvmp-recovery.md'
      return ok(
        JSON.stringify(
          { requestId, url: entry.url, score, verdict, recommendation, playbook, signals },
          null,
          2
        )
      )
    }
  )
}
