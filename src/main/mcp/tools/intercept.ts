import { z } from 'zod'
import { randomUUID } from 'node:crypto'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import {
  getActiveTarget,
  getInterceptRules,
  setInterceptRules,
  applyFetchIntercept,
  getPendingFetchRequests,
  type InterceptRule
} from '../../chrome-cdp'
import { getRequest } from '../../traffic-store'
import { ok, err, errorMessage } from '../utils'

export function registerInterceptTools(mcp: McpServer) {
  mcp.registerTool(
    'intercept_add',
    {
      description:
        'Add a network intercept rule. mode=log: auto-continues and records; mode=block: cancels the request; mode=modify: holds the request for manual intercept_continue/fulfill/fail.',
      inputSchema: {
        urlPattern: z.string().describe('CDP wildcard URL pattern, e.g. "*/api/v1/*"'),
        mode: z.enum(['log', 'block', 'modify']).describe('Intercept mode'),
        stage: z.enum(['Request', 'Response']).optional().describe('Intercept stage (default: Request)'),
        modifyHeaders: z.record(z.string(), z.string()).optional().describe('Headers to inject (modify mode)'),
        replaceBody: z.string().optional().describe('Body to replace (modify mode)')
      }
    },
    async ({ urlPattern, mode, stage = 'Request', modifyHeaders, replaceBody }) => {
      const rules = getInterceptRules()
      const rule: InterceptRule = {
        id: randomUUID(),
        urlPattern,
        mode,
        stage,
        modifyHeaders,
        replaceBody
      }
      setInterceptRules([...rules, rule])
      await applyFetchIntercept()
      return ok(JSON.stringify({ id: rule.id, urlPattern, mode, stage }, null, 2))
    }
  )

  mcp.registerTool(
    'intercept_list',
    {
      description: 'List all active intercept rules.'
    },
    async () => {
      return ok(JSON.stringify(getInterceptRules(), null, 2))
    }
  )

  mcp.registerTool(
    'intercept_remove',
    {
      description: 'Remove an intercept rule by ID.',
      inputSchema: {
        id: z.string().describe('Rule ID from intercept_list')
      }
    },
    async ({ id }) => {
      const rules = getInterceptRules()
      const idx = rules.findIndex((r) => r.id === id)
      if (idx === -1) return err(`unknown rule id: ${id}`)
      const updated = rules.filter((r) => r.id !== id)
      setInterceptRules(updated)
      await applyFetchIntercept()
      return ok(`rule ${id} removed`)
    }
  )

  mcp.registerTool(
    'intercept_pending',
    {
      description: 'List requests currently paused by a modify-mode intercept rule.'
    },
    async () => {
      const pending = getPendingFetchRequests()
      const items = [...pending.entries()].map(([id, p]) => ({
        requestId: id,
        url: p.request.url,
        method: p.request.method,
        resourceType: p.resourceType
      }))
      return ok(JSON.stringify(items, null, 2))
    }
  )

  mcp.registerTool(
    'intercept_continue',
    {
      description: 'Continue a paused request (optionally overriding headers, body, URL, or method).',
      inputSchema: {
        requestId: z.string().describe('Fetch requestId from intercept_pending'),
        headers: z.array(z.object({ name: z.string(), value: z.string() })).optional().describe('Override request headers'),
        postData: z.string().optional().describe('Override request body'),
        url: z.string().optional().describe('Override URL'),
        method: z.string().optional().describe('Override HTTP method')
      }
    },
    async ({ requestId, headers, postData, url, method }) => {
      const target = getActiveTarget()
      if (!target) return err('no active target')
      try {
        await target.dbg.sendCommand('Fetch.continueRequest', {
          requestId,
          ...(headers ? { headers } : {}),
          ...(postData ? { postData: Buffer.from(postData).toString('base64') } : {}),
          ...(url ? { url } : {}),
          ...(method ? { method } : {})
        })
        getPendingFetchRequests().delete(requestId)
        return ok(`continued ${requestId}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'intercept_fulfill',
    {
      description: 'Fulfill a paused request with a custom response.',
      inputSchema: {
        requestId: z.string().describe('Fetch requestId from intercept_pending'),
        responseCode: z.number().int().describe('HTTP status code'),
        headers: z.array(z.object({ name: z.string(), value: z.string() })).optional().describe('Response headers'),
        body: z.string().optional().describe('Response body (will be base64-encoded)')
      }
    },
    async ({ requestId, responseCode, headers, body }) => {
      const target = getActiveTarget()
      if (!target) return err('no active target')
      try {
        await target.dbg.sendCommand('Fetch.fulfillRequest', {
          requestId,
          responseCode,
          ...(headers ? { responseHeaders: headers } : {}),
          ...(body ? { body: Buffer.from(body).toString('base64') } : {})
        })
        getPendingFetchRequests().delete(requestId)
        return ok(`fulfilled ${requestId} with ${responseCode}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'intercept_fail',
    {
      description: 'Fail (abort) a paused request.',
      inputSchema: {
        requestId: z.string().describe('Fetch requestId from intercept_pending'),
        errorReason: z.string().optional().describe('CDP error reason (default: BlockedByClient)')
      }
    },
    async ({ requestId, errorReason = 'BlockedByClient' }) => {
      const target = getActiveTarget()
      if (!target) return err('no active target')
      try {
        await target.dbg.sendCommand('Fetch.failRequest', { requestId, errorReason })
        getPendingFetchRequests().delete(requestId)
        return ok(`failed ${requestId}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'replay_request',
    {
      description:
        'Replay a captured request via Node fetch (independent of the browser). Cookies are pulled from the page and injected automatically.',
      inputSchema: {
        requestId: z.string().describe('requestId to replay'),
        overrides: z
          .object({
            url: z.string().optional(),
            method: z.string().optional(),
            headers: z.record(z.string(), z.string()).optional(),
            body: z.string().optional()
          })
          .optional()
          .describe('Optional overrides for the replayed request')
      }
    },
    async ({ requestId, overrides }) => {
      const entry = getRequest(requestId)
      if (!entry) return err(`unknown requestId: ${requestId}`)
      const target = getActiveTarget()

      // Collect cookies for the target URL
      let cookieHeader = ''
      if (target) {
        try {
          const origin = new URL(overrides?.url ?? entry.url).origin
          const cookieRes = (await target.dbg.sendCommand('Network.getCookies', {
            urls: [origin]
          })) as { cookies: Array<{ name: string; value: string }> }
          if (cookieRes.cookies.length > 0) {
            cookieHeader = cookieRes.cookies.map((c) => `${c.name}=${c.value}`).join('; ')
          }
        } catch {}
      }

      const headers: Record<string, string> = {
        ...(entry.requestHeaders ?? {}),
        ...((overrides?.headers ?? {}) as Record<string, string>)
      }
      if (cookieHeader) headers['cookie'] = cookieHeader

      try {
        const resp = await fetch(overrides?.url ?? entry.url, {
          method: overrides?.method ?? entry.method,
          headers,
          body: overrides?.body ?? entry.requestPostData ?? undefined
        })
        const responseText = await resp.text()
        const respHeaders: Record<string, string> = {}
        resp.headers.forEach((v, k) => { respHeaders[k] = v })
        return ok(
          JSON.stringify(
            {
              status: resp.status,
              statusText: resp.statusText,
              headers: respHeaders,
              body: responseText.slice(0, 10000),
              truncated: responseText.length > 10000
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
