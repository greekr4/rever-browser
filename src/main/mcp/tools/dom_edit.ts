import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getActiveTarget } from '../../chrome-cdp'
import { ok, err, errorMessage } from '../utils'

async function runExpr(expr: string): Promise<string> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')
  const r = (await target.dbg.sendCommand('Runtime.evaluate', {
    expression: expr,
    returnByValue: true
  })) as { result: { value?: string }; exceptionDetails?: { text: string } }
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.text)
  return r.result.value ?? ''
}

export function registerDomEditTools(mcp: McpServer) {
  mcp.registerTool(
    'dom_set_attr',
    {
      description:
        'Set an attribute on the first element matching a CSS selector. Useful for CSS-puzzle challenges (e.g. moving an element via style.left).',
      inputSchema: {
        selector: z.string(),
        attr: z.string(),
        value: z.string()
      }
    },
    async ({ selector, attr, value }) => {
      try {
        const expr = `(() => {
          const el = document.querySelector(${JSON.stringify(selector)});
          if (!el) return 'not-found';
          el.setAttribute(${JSON.stringify(attr)}, ${JSON.stringify(value)});
          return 'ok';
        })()`
        const r = await runExpr(expr)
        if (r === 'not-found') return err(`no element matches selector: ${selector}`)
        return ok(`${selector}[${attr}]=${value}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'dom_set_style',
    {
      description: 'Set an inline CSS style property on the first match.',
      inputSchema: { selector: z.string(), property: z.string(), value: z.string() }
    },
    async ({ selector, property, value }) => {
      try {
        const expr = `(() => {
          const el = document.querySelector(${JSON.stringify(selector)});
          if (!el) return 'not-found';
          el.style.setProperty(${JSON.stringify(property)}, ${JSON.stringify(value)});
          return 'ok';
        })()`
        const r = await runExpr(expr)
        if (r === 'not-found') return err(`no element matches selector: ${selector}`)
        return ok(`${selector}.style.${property}=${value}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'dom_set_text',
    {
      description: 'Set textContent on the first match.',
      inputSchema: { selector: z.string(), text: z.string() }
    },
    async ({ selector, text }) => {
      try {
        const expr = `(() => {
          const el = document.querySelector(${JSON.stringify(selector)});
          if (!el) return 'not-found';
          el.textContent = ${JSON.stringify(text)};
          return 'ok';
        })()`
        const r = await runExpr(expr)
        if (r === 'not-found') return err(`no element matches: ${selector}`)
        return ok(`text set`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'dom_remove',
    {
      description: 'Remove all elements matching a CSS selector from the DOM.',
      inputSchema: { selector: z.string() }
    },
    async ({ selector }) => {
      try {
        const expr = `(() => {
          const els = document.querySelectorAll(${JSON.stringify(selector)});
          els.forEach((e) => e.remove());
          return els.length;
        })()`
        const r = await runExpr(expr)
        return ok(`removed ${r} element(s)`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
