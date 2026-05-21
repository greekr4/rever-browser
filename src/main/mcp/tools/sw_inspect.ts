import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getActiveTarget } from '../../chrome-cdp'
import { ok, err, errorMessage } from '../utils'

async function runExpr(expr: string): Promise<unknown> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')
  const r = (await target.dbg.sendCommand('Runtime.evaluate', {
    expression: expr,
    awaitPromise: true,
    returnByValue: true
  })) as { result: { value?: unknown }; exceptionDetails?: { text: string } }
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.text)
  return r.result.value
}

export function registerServiceWorkerTools(mcp: McpServer) {
  mcp.registerTool(
    'sw_list',
    {
      description:
        'List Service Worker registrations on the active page (scope, active script URL, state).'
    },
    async () => {
      try {
        const expr = `(async () => {
          if (!navigator.serviceWorker) return { error: 'not supported' };
          const regs = await navigator.serviceWorker.getRegistrations();
          return regs.map(r => ({
            scope: r.scope,
            active: r.active ? { scriptURL: r.active.scriptURL, state: r.active.state } : null,
            waiting: r.waiting ? { scriptURL: r.waiting.scriptURL, state: r.waiting.state } : null,
            installing: r.installing ? { scriptURL: r.installing.scriptURL, state: r.installing.state } : null
          }));
        })()`
        const result = await runExpr(expr)
        return ok(JSON.stringify(result, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'sw_unregister',
    {
      description: 'Unregister a Service Worker by scope. Useful when SW caching interferes with reversing.',
      inputSchema: { scope: z.string().describe('Scope URL') }
    },
    async ({ scope }) => {
      try {
        const expr = `(async () => {
          const reg = await navigator.serviceWorker.getRegistration(${JSON.stringify(scope)});
          if (!reg) return 'not-found';
          return await reg.unregister() ? 'ok' : 'failed';
        })()`
        const r = await runExpr(expr)
        return ok(`unregister(${scope}) → ${r}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'sw_caches',
    {
      description: 'List Cache API entries (Service Worker / fetch caches) for the active page.'
    },
    async () => {
      try {
        const expr = `(async () => {
          if (!('caches' in self)) return { error: 'caches API not available' };
          const names = await caches.keys();
          const out = {};
          for (const n of names) {
            const c = await caches.open(n);
            const reqs = await c.keys();
            out[n] = reqs.map(r => r.url);
          }
          return out;
        })()`
        const r = await runExpr(expr)
        return ok(JSON.stringify(r, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
