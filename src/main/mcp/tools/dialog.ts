import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import {
  clearDialogHistory,
  getActiveTarget,
  getDialogAutoDismiss,
  getDialogHistory,
  setDialogAutoDismiss
} from '../../chrome-cdp'
import { ok, err, errorMessage } from '../utils'

export function registerDialogTools(mcp: McpServer) {
  mcp.registerTool(
    'dialog_history',
    {
      description:
        'List recent JavaScript dialog events (alert/confirm/prompt) that the page tried to open. Useful to see what message a challenge popped — the agent never sees the modal because rever-browser auto-dismisses, but the content is recorded here. Also queries the in-page history captured by the script override.',
      inputSchema: {
        limit: z.number().int().positive().max(200).optional().describe('Default 50')
      }
    },
    async ({ limit }) => {
      // CDP-side history (server-recorded)
      const cdpHistory = getDialogHistory(limit ?? 50)
      // Page-side history (from the injected override)
      let pageHistory: unknown[] = []
      const target = getActiveTarget()
      if (target) {
        try {
          const r = (await target.dbg.sendCommand('Runtime.evaluate', {
            expression: 'JSON.stringify(window.__revDialogHistory || [])',
            returnByValue: true
          })) as { result: { value?: string } }
          pageHistory = JSON.parse(r.result.value ?? '[]')
        } catch {}
      }
      return ok(
        JSON.stringify(
          {
            autoDismiss: getDialogAutoDismiss(),
            cdpHistory,
            pageHistory
          },
          null,
          2
        )
      )
    }
  )

  mcp.registerTool(
    'dialog_set_auto_dismiss',
    {
      description:
        'Toggle whether native JS dialogs are auto-dismissed. Default true (recommended). Turn off if you want to see real alert/confirm/prompt modals.',
      inputSchema: { enabled: z.boolean() }
    },
    async ({ enabled }) => {
      setDialogAutoDismiss(enabled)
      return ok(`auto-dismiss = ${enabled}`)
    }
  )

  mcp.registerTool(
    'dialog_clear_history',
    { description: 'Clear the CDP-side dialog history buffer.' },
    async () => {
      clearDialogHistory()
      return ok('cleared')
    }
  )

  mcp.registerTool(
    'dialog_inject_override',
    {
      description:
        'Re-inject the alert/confirm/prompt override into the active page. Useful if a page replaced window.alert with its own implementation after load.'
    },
    async () => {
      const target = getActiveTarget()
      if (!target) return err('no active target')
      const expr = `(() => {
        window.__revDialogOverride = undefined;
        window.__revDialogHistory = window.__revDialogHistory || [];
        function record(type, message, def) {
          const entry = { type, message: String(message ?? ''), ts: Date.now() };
          if (def !== undefined) entry.default = String(def);
          window.__revDialogHistory.push(entry);
          try { console.log('[rev-' + type + ']', message); } catch(e) {}
        }
        window.alert = function(msg) { record('alert', msg); };
        window.confirm = function(msg) { record('confirm', msg); return true; };
        window.prompt = function(msg, def) { record('prompt', msg, def); return def == null ? '' : String(def); };
        return 'ok';
      })()`
      try {
        await target.dbg.sendCommand('Runtime.evaluate', { expression: expr })
        return ok('re-injected dialog override on current page')
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
