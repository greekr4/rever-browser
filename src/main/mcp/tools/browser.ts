import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { emitAiAction } from '../../ai-events'
import { getActiveTarget, waitForSettle } from '../../chrome-cdp'
import { setViewport } from '../../viewport'
import { evalInPage } from '../cdp-eval'
import { humanScroll } from '../human-input'
import { clickRef, takeSnapshot, typeRef } from '../snapshot'
import { ok, err, errorMessage } from '../utils'

/**
 * Wait for the page to settle, then take a fresh snapshot. Used by
 * click/type/navigate to bundle the post-action page state into the same
 * tool response — saves the agent a separate browser_snapshot round-trip
 * and makes the previous snapshot's refs explicitly stale.
 */
async function snapshotAfter(actionResult: string): Promise<{
  content: { type: 'text'; text: string }[]
}> {
  try {
    await waitForSettle()
    const snap = await takeSnapshot()
    return ok(
      `${actionResult}\n\n--- snapshot (refs from previous snapshot are now stale) ---\nurl: ${snap.url}\ntitle: ${snap.title}\n\n${snap.tree}`
    )
  } catch (e) {
    return ok(`${actionResult}\n\n[auto-snapshot failed: ${errorMessage(e)}]`)
  }
}

export function registerBrowserTools(mcp: McpServer) {
  mcp.registerTool(
    'browser_navigate',
    {
      description:
        'Navigate the active browser tab to the given URL. Waits for the load and returns a fresh accessibility snapshot — no separate browser_snapshot needed.',
      inputSchema: {
        url: z.string().describe('Absolute URL (must include scheme, e.g. https://...)')
      }
    },
    async ({ url }) => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target — open a page first')
      try {
        emitAiAction({ kind: 'navigate', label: `AI navigate`, detail: url })
        await target.wc.loadURL(url)
        return await snapshotAfter(`navigated to ${url}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_snapshot',
    {
      description:
        'Capture the current page as a compact accessibility-tree snapshot. Returns url, title, and a YAML-like outline where actionable nodes carry [ref=rN] handles. Use these refs in browser_click and browser_type. This is the primary way to "see" the page — far cheaper than dumping HTML or screenshots.'
    },
    async () => {
      try {
        emitAiAction({ kind: 'snapshot', label: 'AI snapshot' })
        const snap = await takeSnapshot()
        return ok(`url: ${snap.url}\ntitle: ${snap.title}\n\n${snap.tree}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_click',
    {
      description:
        'Click the element identified by ref (from the latest browser_snapshot). Scrolls into view, performs a human-like mouse move, then clicks. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards. Refs from the previous snapshot are now stale.',
      inputSchema: {
        ref: z.string().describe('Element ref from browser_snapshot, e.g. "r12"')
      }
    },
    async ({ ref }) => {
      try {
        await clickRef(ref)
        return await snapshotAfter(`clicked ${ref}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_type',
    {
      description:
        'Move the cursor to the input/textarea identified by ref, click to focus, then type the text with realistic per-keystroke timing. Press Enter with submit=true (default false). Returns a fresh snapshot — DO NOT call browser_snapshot afterwards. Refs from the previous snapshot are now stale.',
      inputSchema: {
        ref: z.string().describe('Element ref from browser_snapshot, e.g. "r7"'),
        text: z.string().describe('Text to put into the field'),
        submit: z
          .boolean()
          .optional()
          .describe('If true, dispatches an Enter keydown after typing (default false)')
      }
    },
    async ({ ref, text, submit }) => {
      try {
        await typeRef(ref, text, submit ?? false)
        return await snapshotAfter(`typed into ${ref}${submit ? ' + submit' : ''}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_scroll',
    {
      description:
        'Scroll the page. Provide either absolute y or relative deltaY (in pixels). Useful for infinite-scroll sites.',
      inputSchema: {
        y: z.number().optional().describe('Absolute scroll position in px from top'),
        deltaY: z.number().optional().describe('Relative scroll delta in px (positive = down)')
      }
    },
    async ({ y, deltaY }) => {
      try {
        emitAiAction({
          kind: 'scroll',
          label: 'AI scroll',
          detail: typeof y === 'number' ? `y=${y}` : `Δy=${deltaY ?? 0}`
        })
        const result = await humanScroll(deltaY ?? 0, y)
        return ok(`scrolled, scrollY=${result.scrollY}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'set_viewport',
    {
      description:
        'Switch the active browser tab between desktop and mobile mode (changes user-agent + device metrics) and reloads the page. Use mobile mode to access mobile-only sites or APIs.',
      inputSchema: {
        mode: z.enum(['desktop', 'mobile']).describe('Target viewport mode')
      }
    },
    async ({ mode }) => {
      try {
        const next = await setViewport(mode)
        return ok(`viewport set to ${next}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_evaluate',
    {
      description:
        'Run a JavaScript expression in the active page and return the result. The expression must return a JSON-serialisable value.',
      inputSchema: {
        expression: z.string().describe('JavaScript expression to evaluate (must return a value)')
      }
    },
    async ({ expression }) => {
      try {
        emitAiAction({
          kind: 'evaluate',
          label: 'AI evaluate',
          detail: expression.slice(0, 80)
        })
        const result = await evalInPage<unknown>(expression)
        return ok(typeof result === 'string' ? result : JSON.stringify(result, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_screenshot',
    {
      description:
        'Capture a PNG screenshot of the current page (visible area). Returned as image content.'
    },
    async () => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target — open a page first')
      try {
        emitAiAction({ kind: 'screenshot', label: 'AI screenshot' })
        const img = await target.wc.capturePage()
        const png = img.toPNG()
        return {
          content: [
            {
              type: 'image' as const,
              data: png.toString('base64'),
              mimeType: 'image/png'
            }
          ]
        }
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
