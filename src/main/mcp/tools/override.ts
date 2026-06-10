import { z } from 'zod'
import { randomUUID } from 'node:crypto'
import { readFileSync, statSync } from 'node:fs'
import path from 'node:path'

import { app } from 'electron'
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import {
  getInterceptRules,
  setInterceptRules,
  applyFetchIntercept,
  type InterceptRule
} from '../../chrome-cdp'
import { ok, err, errorMessage } from '../utils'

const OVERRIDE_FILE_SIZE_LIMIT = 5 * 1024 * 1024 // 5MB

// 허용 디렉터리: <userData>/overrides
function getAllowedOverrideDir(): string {
  return path.join(app.getPath('userData'), 'overrides')
}

// 경로 탈출 여부 검사. 허용 디렉터리 밖이면 에러 메시지 반환.
function resolveOverridePath(file: string): { resolved: string } | { error: string } {
  const allowedDir = getAllowedOverrideDir()
  const resolved = path.resolve(file)
  if (!resolved.startsWith(allowedDir + path.sep) && resolved !== allowedDir) {
    return { error: `file path is outside the allowed override directory (${allowedDir})` }
  }
  return { resolved }
}

// Local store of override rule IDs so we can list "just overrides" separately
// from other intercept rules.
const overrideIds = new Set<string>()

export function registerOverrideTools(mcp: McpServer) {
  mcp.registerTool(
    'override_add',
    {
      description:
        'Register a local override: when a URL matching `urlPattern` is requested, fulfill it with `body` (or contents of `file`) instead of hitting the network. Equivalent to DevTools "Local Overrides". Use to neutralize anti-bot scripts, replace a bundle with a beautified copy, or patch a comparison.',
      inputSchema: {
        urlPattern: z.string().describe('CDP wildcard pattern, e.g. "*/static/main.js"'),
        body: z.string().optional().describe('Inline replacement body'),
        file: z.string().optional().describe('Local file path to load as the body (mutually exclusive with body)'),
        mimeType: z.string().optional()
      }
    },
    async ({ urlPattern, body, file, mimeType }) => {
      try {
        if (body == null && !file) return err('provide body or file')
        let text: string
        if (file) {
          const check = resolveOverridePath(file)
          if ('error' in check) return err(check.error)
          try {
            const stat = statSync(check.resolved)
            if (stat.size > OVERRIDE_FILE_SIZE_LIMIT) {
              return err(`file exceeds size limit (${OVERRIDE_FILE_SIZE_LIMIT} bytes)`)
            }
          } catch (e) {
            return err(`cannot stat file: ${errorMessage(e)}`)
          }
          text = readFileSync(check.resolved, 'utf8')
        } else {
          text = body ?? ''
        }
        const rule: InterceptRule = {
          id: randomUUID(),
          urlPattern,
          mode: 'modify',
          stage: 'Request',
          replaceBody: text,
          ...(mimeType ? { modifyHeaders: { 'content-type': mimeType } } : {})
        }
        setInterceptRules([...getInterceptRules(), rule])
        overrideIds.add(rule.id)
        await applyFetchIntercept()
        return ok(JSON.stringify({ id: rule.id, urlPattern, bytes: text.length }, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'override_list',
    { description: 'List active local overrides.' },
    async () => {
      const rules = getInterceptRules().filter((r) => overrideIds.has(r.id))
      return ok(
        JSON.stringify(
          rules.map((r) => ({
            id: r.id,
            urlPattern: r.urlPattern,
            bytes: r.replaceBody?.length ?? 0,
            mimeType: r.modifyHeaders?.['content-type']
          })),
          null,
          2
        )
      )
    }
  )

  mcp.registerTool(
    'override_remove',
    {
      description: 'Remove a local override by ID.',
      inputSchema: { id: z.string() }
    },
    async ({ id }) => {
      if (!overrideIds.has(id)) return err(`not an override: ${id}`)
      setInterceptRules(getInterceptRules().filter((r) => r.id !== id))
      overrideIds.delete(id)
      await applyFetchIntercept()
      return ok(`removed ${id}`)
    }
  )
}
