import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getRequest } from '../../traffic-store'
import { ok, err, errorMessage } from '../utils'

type Packer = 'jsfuck' | 'aaencode' | 'jjencode' | 'packer' | 'hex-array' | 'webpack' | 'eval-chain' | 'unknown'

interface DetectResult {
  packer: Packer
  confidence: number
  evidence: string[]
}

function detect(src: string): DetectResult {
  const ev: string[] = []
  // JSFuck: only +[]!()
  if (/^[+\-\[\]!()]+$/m.test(src.replace(/\s/g, '').slice(0, 500))) {
    return { packer: 'jsfuck', confidence: 0.9, evidence: ['characters limited to +[]!() pattern'] }
  }
  // AAencode: ﾟωﾟ, ﾟДﾟ etc.
  if (/[ﾟωＤノ()_+]/.test(src) && /ﾟωﾟノ|ﾟДﾟ/.test(src)) {
    return { packer: 'aaencode', confidence: 0.95, evidence: ['ﾟωﾟ ﾟДﾟ symbols'] }
  }
  // JJencode: $=
  if (/^\s*\$=~\[\]/.test(src) || /\$=\{\}|\$\$\$\$\$=/.test(src.slice(0, 500))) {
    return { packer: 'jjencode', confidence: 0.85, evidence: ['$= header'] }
  }
  // Dean Edwards packer / eval(function(p,a,c,k,e,d))
  if (/eval\(\s*function\(p,a,c,k,e,[dr]\)/.test(src)) {
    return {
      packer: 'packer',
      confidence: 0.95,
      evidence: ['eval(function(p,a,c,k,e,d) header']
    }
  }
  // Hex-array obfuscation: large `var _0x = ["...", "..."]`
  const hexArr = src.match(/var\s+(_0x[a-f0-9]+)\s*=\s*\[[^\]]*?\][;,]/)
  if (hexArr) {
    return {
      packer: 'hex-array',
      confidence: 0.85,
      evidence: ['hex-named string array', hexArr[0].slice(0, 80)]
    }
  }
  if (/__webpack_require__|webpackJsonp/.test(src)) {
    ev.push('webpack runtime present')
    return { packer: 'webpack', confidence: 0.7, evidence: ev }
  }
  if (/\beval\s*\(/.test(src) && src.length > 10_000) {
    return {
      packer: 'eval-chain',
      confidence: 0.4,
      evidence: ['eval() call in large file — may be self-decoding']
    }
  }
  return { packer: 'unknown', confidence: 0, evidence: [] }
}

export function registerDeobTools(mcp: McpServer) {
  mcp.registerTool(
    'deob_detect',
    {
      description:
        'Detect which obfuscation packer (JSFuck / AAencode / JJencode / Dean Edwards / hex-array / webpack) a captured JS file uses, so the right unpacker can be picked.',
      inputSchema: {
        requestId: z.string().describe('requestId of a captured Script')
      }
    },
    async ({ requestId }) => {
      try {
        const r = getRequest(requestId)
        if (!r?.responseBody) return err(`no body for ${requestId}`)
        const result = detect(r.responseBody)
        return ok(JSON.stringify({ url: r.url, bytes: r.responseBody.length, ...result }, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'deob_auto',
    {
      description:
        'Auto-deobfuscate a captured JS file: detects packer, then chains the appropriate unpackers (webcrack for general obfuscation, regex-based for hex-array, eval-unwrap for Dean Edwards packer).',
      inputSchema: {
        requestId: z.string()
      }
    },
    async ({ requestId }) => {
      try {
        const r = getRequest(requestId)
        if (!r?.responseBody) return err(`no body for ${requestId}`)
        const src = r.responseBody
        const det = detect(src)
        const steps: Array<{ tool: string; bytesBefore: number; bytesAfter: number; note?: string }> = []
        let cur = src

        // 1. webcrack (handles webpack/general obfuscation + sourcemaps)
        if (det.packer === 'webpack' || det.packer === 'hex-array' || det.packer === 'unknown') {
          try {
            const { runWebcrack } = await import('../script-analysis')
            const out = await runWebcrack(cur)
            steps.push({
              tool: 'webcrack',
              bytesBefore: cur.length,
              bytesAfter: out.length,
              note: 'general deobfuscation'
            })
            cur = out
          } catch (e) {
            steps.push({
              tool: 'webcrack',
              bytesBefore: cur.length,
              bytesAfter: cur.length,
              note: `failed: ${errorMessage(e)}`
            })
          }
        }

        // 2. Dean Edwards packer: replace eval with print
        if (det.packer === 'packer') {
          const m = cur.match(/^eval\((.*)\)\s*$/s)
          if (m) {
            steps.push({
              tool: 'packer-unwrap',
              bytesBefore: cur.length,
              bytesAfter: m[1].length,
              note: 'replaced outer eval(...) with body'
            })
            cur = m[1]
          }
        }

        return ok(
          JSON.stringify(
            {
              url: r.url,
              detected: det,
              steps,
              finalBytes: cur.length,
              preview: cur.slice(0, 2000)
            },
            null,
            2
          )
        )
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
