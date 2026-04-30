import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getActiveTarget } from '../../chrome-cdp'
import { listRequests } from '../../traffic-store'
import { ok, err, errorMessage } from '../utils'

const AUTH_HEADERS = ['authorization', 'cookie', 'x-csrf-token', 'x-api-key']

export function registerAuthTools(mcp: McpServer) {
  mcp.registerTool(
    'auth_dump',
    {
      description:
        'Dump authentication state for an origin: cookies, localStorage, sessionStorage, and recent auth-related request headers (Authorization, Cookie, X-CSRF-Token, X-API-Key).',
      inputSchema: {
        origin: z
          .string()
          .optional()
          .describe('Origin URL (e.g. "https://example.com"). Defaults to active page origin.')
      }
    },
    async ({ origin }) => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target — open a page first')
      try {
        // Resolve origin
        let resolvedOrigin = origin
        if (!resolvedOrigin) {
          const urlResult = (await target.dbg.sendCommand('Runtime.evaluate', {
            expression: 'location.origin',
            returnByValue: true
          })) as { result: { value?: string } }
          resolvedOrigin = urlResult.result.value ?? ''
        }

        // Cookies
        const cookieRes = (await target.dbg.sendCommand('Network.getCookies', {
          urls: [resolvedOrigin]
        })) as { cookies: unknown[] }

        // localStorage + sessionStorage
        const storageExpr = `JSON.stringify({
          ls: (() => { try { return Object.fromEntries(Object.entries(localStorage)) } catch(e) { return {} } })(),
          ss: (() => { try { return Object.fromEntries(Object.entries(sessionStorage)) } catch(e) { return {} } })()
        })`
        const storageResult = (await target.dbg.sendCommand('Runtime.evaluate', {
          expression: storageExpr,
          returnByValue: true
        })) as { result: { value?: string } }
        const storage = JSON.parse(storageResult.result.value ?? '{"ls":{},"ss":{}}')

        // Recent auth headers from traffic-store
        let host = ''
        try {
          host = new URL(resolvedOrigin).host
        } catch {}
        const recentRequests = listRequests({ host, limit: 20 })
        const headerHits: Array<{ requestId: string; header: string; value: string }> = []
        for (const req of recentRequests) {
          if (!req.requestHeaders) continue
          for (const h of AUTH_HEADERS) {
            const val = req.requestHeaders[h] ?? req.requestHeaders[h.toLowerCase()]
            if (val) headerHits.push({ requestId: req.requestId, header: h, value: val })
          }
        }

        return ok(
          JSON.stringify(
            {
              origin: resolvedOrigin,
              cookies: cookieRes.cookies,
              localStorage: storage.ls,
              sessionStorage: storage.ss,
              recentAuthHeaders: headerHits
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

  mcp.registerTool(
    'export_python_client',
    {
      description:
        'Generate a Python code snippet to reproduce a captured HTTP request using requests or httpx.',
      inputSchema: {
        requestId: z.string().describe('requestId to reproduce'),
        library: z
          .enum(['requests', 'httpx'])
          .optional()
          .describe('Python library to use (default: requests)')
      }
    },
    async ({ requestId, library = 'requests' }) => {
      const { getRequest } = await import('../../traffic-store')
      const entry = getRequest(requestId)
      if (!entry) return err(`unknown requestId: ${requestId}`)

      const lib = library === 'httpx' ? 'httpx' : 'requests'
      const clientClass = lib === 'httpx' ? 'httpx.Client' : 'requests.Session'

      // Extract cookies from headers
      const headers = { ...(entry.requestHeaders ?? {}) }
      const cookieHeader = headers['cookie'] ?? headers['Cookie'] ?? ''
      delete headers['cookie']
      delete headers['Cookie']

      const cookiesObj: Record<string, string> = {}
      if (cookieHeader) {
        for (const part of cookieHeader.split(';')) {
          const idx = part.indexOf('=')
          if (idx === -1) continue
          cookiesObj[part.slice(0, idx).trim()] = part.slice(idx + 1).trim()
        }
      }

      const lines: string[] = [
        `import ${lib}`,
        '',
        `s = ${clientClass}()`,
        `s.headers.update(${JSON.stringify(headers, null, 4)})`
      ]

      if (Object.keys(cookiesObj).length > 0) {
        lines.push(`s.cookies.update(${JSON.stringify(cookiesObj, null, 4)})`)
      }

      const method = entry.method.toLowerCase()
      const hasBody = entry.requestPostData != null
      if (hasBody) {
        lines.push(`data = ${JSON.stringify(entry.requestPostData)}`)
        lines.push(`resp = s.${method}(${JSON.stringify(entry.url)}, data=data)`)
      } else {
        lines.push(`resp = s.${method}(${JSON.stringify(entry.url)})`)
      }

      lines.push('print(resp.status_code, resp.text[:500])')

      return ok(lines.join('\n'))
    }
  )
}
