import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { listRequests, getRequest } from '../../traffic-store'
import { ok, err, errorMessage } from '../utils'

interface SecurityIssue {
  severity: 'info' | 'low' | 'medium' | 'high'
  header: string
  message: string
  current?: string
}

const SECURITY_HEADERS = [
  'content-security-policy',
  'strict-transport-security',
  'x-frame-options',
  'x-content-type-options',
  'referrer-policy',
  'permissions-policy',
  'cross-origin-opener-policy',
  'cross-origin-resource-policy',
  'cross-origin-embedder-policy'
]

function analyze(headers: Record<string, string>): SecurityIssue[] {
  const lc: Record<string, string> = {}
  for (const [k, v] of Object.entries(headers)) lc[k.toLowerCase()] = v

  const out: SecurityIssue[] = []

  const csp = lc['content-security-policy']
  if (!csp) {
    out.push({ severity: 'high', header: 'content-security-policy', message: 'no CSP set' })
  } else {
    if (csp.includes("'unsafe-inline'"))
      out.push({ severity: 'medium', header: 'csp', message: "CSP allows 'unsafe-inline'", current: csp })
    if (csp.includes("'unsafe-eval'"))
      out.push({ severity: 'medium', header: 'csp', message: "CSP allows 'unsafe-eval'", current: csp })
    if (csp.includes('*'))
      out.push({ severity: 'low', header: 'csp', message: 'CSP uses wildcard', current: csp })
  }

  const hsts = lc['strict-transport-security']
  if (!hsts) {
    out.push({ severity: 'medium', header: 'strict-transport-security', message: 'no HSTS' })
  } else if (!/max-age=\d+/.test(hsts) || /max-age=0/.test(hsts)) {
    out.push({ severity: 'low', header: 'hsts', message: 'HSTS max-age missing or zero', current: hsts })
  }

  if (!lc['x-frame-options'] && !(csp ?? '').includes('frame-ancestors')) {
    out.push({
      severity: 'medium',
      header: 'x-frame-options',
      message: 'clickjacking protection (X-Frame-Options or CSP frame-ancestors) absent'
    })
  }

  if (lc['x-content-type-options']?.toLowerCase() !== 'nosniff') {
    out.push({
      severity: 'low',
      header: 'x-content-type-options',
      message: 'missing nosniff',
      current: lc['x-content-type-options']
    })
  }

  if (lc['access-control-allow-origin'] === '*' && lc['access-control-allow-credentials'] === 'true') {
    out.push({
      severity: 'high',
      header: 'cors',
      message: 'wildcard ACAO + credentials is invalid and may leak'
    })
  }

  return out
}

export function registerSecurityInspectTools(mcp: McpServer) {
  mcp.registerTool(
    'security_inspect',
    {
      description:
        'Inspect security-relevant response headers for the active page (or a specific URL). Reports CSP/HSTS/XFO/CORS issues.',
      inputSchema: {
        url: z.string().optional().describe('URL to inspect (defaults to most recent Document request)')
      }
    },
    async ({ url }) => {
      try {
        const candidates = listRequests({ limit: 200 }).filter((r) =>
          url ? r.url === url : r.resourceType === 'Document'
        )
        if (candidates.length === 0) return err('no matching request in traffic store')
        const r = candidates[0]
        const full = getRequest(r.requestId)
        const headers = full?.responseHeaders ?? {}
        const issues = analyze(headers)
        const present: Record<string, string> = {}
        const lc: Record<string, string> = {}
        for (const [k, v] of Object.entries(headers)) lc[k.toLowerCase()] = v
        for (const h of SECURITY_HEADERS) if (lc[h] != null) present[h] = lc[h]
        return ok(JSON.stringify({ url: r.url, securityHeaders: present, issues }, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
