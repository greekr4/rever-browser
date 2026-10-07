import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getActiveTarget } from '../../chrome-cdp'
import { listRequests, getRequest } from '../../traffic-store'
import { generateCode } from '../codegen'
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
      const entry = getRequest(requestId)
      if (!entry) return err(`unknown requestId: ${requestId}`)
      // Delegate to the shared generator. Keep secrets visible here (runnable)
      // for back-compat; use export_client with maskSecrets for a shareable one.
      return ok(
        generateCode(entry, { lang: 'python', pythonLibrary: library, maskSecrets: false })
      )
    }
  )

  mcp.registerTool(
    'export_client',
    {
      description:
        'Generate runnable code to reproduce a captured request in curl, Python (requests/httpx) or TypeScript (fetch). Credentials (Authorization, Cookie, API keys, session cookies) are MASKED by default so the output is safe to share — pass maskSecrets=false for a runnable copy. Fixes JSON-body handling that export_python_client got wrong.',
      inputSchema: {
        requestId: z.string().describe('requestId to reproduce'),
        lang: z.enum(['curl', 'python', 'typescript']).describe('Output language'),
        maskSecrets: z
          .boolean()
          .optional()
          .describe('Mask credential values (default true — safe to share)'),
        pythonLibrary: z.enum(['requests', 'httpx']).optional().describe('Python only (default requests)')
      }
    },
    async ({ requestId, lang, maskSecrets, pythonLibrary }) => {
      const entry = getRequest(requestId)
      if (!entry) return err(`unknown requestId: ${requestId}`)
      return ok(generateCode(entry, { lang, maskSecrets, pythonLibrary }))
    }
  )
}
