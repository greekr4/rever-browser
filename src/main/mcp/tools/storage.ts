import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getActiveTarget } from '../../chrome-cdp'
import { ok, err, errorMessage } from '../utils'

async function evalString(expr: string): Promise<string | null> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target — open a page first')
  const r = (await target.dbg.sendCommand('Runtime.evaluate', {
    expression: expr,
    returnByValue: true
  })) as { result: { value?: string } }
  return r.result.value ?? null
}

export function registerStorageTools(mcp: McpServer) {
  mcp.registerTool(
    'cookie_set',
    {
      description:
        'Set or overwrite a cookie on the active page. Use to tamper with auth/session/level cookies for testing.',
      inputSchema: {
        name: z.string(),
        value: z.string(),
        url: z.string().optional().describe('Cookie URL/origin (defaults to active page origin)'),
        domain: z.string().optional(),
        path: z.string().optional(),
        secure: z.boolean().optional(),
        httpOnly: z.boolean().optional(),
        sameSite: z.enum(['Strict', 'Lax', 'None']).optional(),
        expires: z.number().optional().describe('Unix seconds (omit for session)')
      }
    },
    async ({ name, value, url, domain, path, secure, httpOnly, sameSite, expires }) => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        const resolved = url ?? (await evalString('location.origin')) ?? ''
        const res = (await target.dbg.sendCommand('Network.setCookie', {
          name,
          value,
          ...(resolved ? { url: resolved } : {}),
          ...(domain ? { domain } : {}),
          ...(path ? { path } : {}),
          ...(secure != null ? { secure } : {}),
          ...(httpOnly != null ? { httpOnly } : {}),
          ...(sameSite ? { sameSite } : {}),
          ...(expires != null ? { expires } : {})
        })) as { success: boolean }
        return ok(JSON.stringify({ name, success: res.success }, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'cookie_delete',
    {
      description: 'Delete cookies by name (optionally scoped to url/domain/path).',
      inputSchema: {
        name: z.string(),
        url: z.string().optional(),
        domain: z.string().optional(),
        path: z.string().optional()
      }
    },
    async ({ name, url, domain, path }) => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        await target.dbg.sendCommand('Network.deleteCookies', {
          name,
          ...(url ? { url } : {}),
          ...(domain ? { domain } : {}),
          ...(path ? { path } : {})
        })
        return ok(`deleted cookie ${name}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'cookie_list',
    {
      description: 'List cookies for given URLs (or the active origin).',
      inputSchema: {
        urls: z.array(z.string()).optional()
      }
    },
    async ({ urls }) => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        const resolved =
          urls && urls.length ? urls : [((await evalString('location.origin')) as string) ?? '']
        const res = (await target.dbg.sendCommand('Network.getCookies', {
          urls: resolved
        })) as { cookies: unknown[] }
        return ok(JSON.stringify(res.cookies, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  for (const kind of ['local', 'session'] as const) {
    const storage = `${kind}Storage`

    mcp.registerTool(
      `${kind}_storage_get`,
      {
        description: `Get all ${storage} keys/values for the active page.`
      },
      async () => {
        try {
          const expr = `JSON.stringify(Object.fromEntries(Object.entries(${storage})))`
          const val = await evalString(expr)
          return ok(val ?? '{}')
        } catch (e) {
          return err(errorMessage(e))
        }
      }
    )

    mcp.registerTool(
      `${kind}_storage_set`,
      {
        description: `Set a key in ${storage}.`,
        inputSchema: { key: z.string(), value: z.string() }
      },
      async ({ key, value }) => {
        try {
          const expr = `(${storage}.setItem(${JSON.stringify(key)}, ${JSON.stringify(value)}), 'ok')`
          await evalString(expr)
          return ok(`${storage}.${key} set`)
        } catch (e) {
          return err(errorMessage(e))
        }
      }
    )

    mcp.registerTool(
      `${kind}_storage_delete`,
      {
        description: `Remove a key from ${storage}.`,
        inputSchema: { key: z.string() }
      },
      async ({ key }) => {
        try {
          await evalString(`(${storage}.removeItem(${JSON.stringify(key)}), 'ok')`)
          return ok(`${storage}.${key} removed`)
        } catch (e) {
          return err(errorMessage(e))
        }
      }
    )

    mcp.registerTool(
      `${kind}_storage_clear`,
      {
        description: `Clear all ${storage} entries for the active page.`
      },
      async () => {
        try {
          await evalString(`(${storage}.clear(), 'ok')`)
          return ok(`${storage} cleared`)
        } catch (e) {
          return err(errorMessage(e))
        }
      }
    )
  }
}
