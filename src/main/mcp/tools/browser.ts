import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { emitAiAction } from '../../ai-events'
import { getActiveTarget, waitForSettle } from '../../chrome-cdp'
import { setViewport } from '../../viewport'
import { evalInPage } from '../cdp-eval'
import { humanScroll } from '../human-input'
import { clickRef, clickSelector, takeSnapshot, typeRef, typeSelector } from '../snapshot'
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
    'browser_click_selector',
    {
      description:
        'Click the first element matching a CSS selector, using the SAME human-shaped cursor move + click as browser_click (visualised on the page, fires trusted events). Use this to interact when you located an element via dom_extract / browser_evaluate and have no snapshot ref — NEVER poke the DOM with raw JS (.click()/dispatchEvent) to interact, as that skips the cursor animation and trips bot detection. Returns a fresh snapshot.',
      inputSchema: {
        selector: z
          .string()
          .describe('CSS selector for the target, e.g. "#q" or "button.search-btn"')
      }
    },
    async ({ selector }) => {
      try {
        await clickSelector(selector)
        return await snapshotAfter(`clicked selector ${selector}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_type_selector',
    {
      description:
        'Type into the first element matching a CSS selector, using the SAME human-shaped focus + per-keystroke timing as browser_type (visualised, fires real trusted key events that survive bot detection). Use when you have a selector but no snapshot ref. Set submit=true to press Enter after typing. Returns a fresh snapshot — do NOT set the value with raw JS.',
      inputSchema: {
        selector: z
          .string()
          .describe('CSS selector for the input/textarea, e.g. "#q"'),
        text: z.string().describe('Text to type into the field'),
        submit: z
          .boolean()
          .optional()
          .describe('If true, presses Enter after typing (default false)')
      }
    },
    async ({ selector, text, submit }) => {
      try {
        await typeSelector(selector, text, submit ?? false)
        return await snapshotAfter(`typed into selector ${selector}${submit ? ' + submit' : ''}`)
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
    'dom_extract',
    {
      description:
        'Extract structured data from the rendered DOM by CSS selector. For each matching element, pulls the requested fields (text/href/src/value/html) plus any named attributes. This is the primary way to scrape results from server-rendered (SSR) pages — no JSON API required. Prefer this over ad-hoc browser_evaluate for pulling lists (search results, tables, cards).',
      inputSchema: {
        selector: z
          .string()
          .describe('CSS selector, e.g. "a.result__title" or "ul.list > li"'),
        fields: z
          .array(z.enum(['text', 'href', 'src', 'value', 'html']))
          .optional()
          .describe("Built-in fields to pull per node. Default ['text']."),
        attrs: z
          .array(z.string())
          .optional()
          .describe('Extra attribute names to read, e.g. ["data-id","aria-label"]'),
        limit: z
          .number()
          .int()
          .min(1)
          .max(500)
          .optional()
          .describe('Max nodes to return (default 50). Guards against huge dumps.')
      }
    },
    async ({ selector, fields, attrs, limit }) => {
      const fieldList = fields && fields.length ? fields : ['text']
      const attrList = attrs ?? []
      const cap = limit ?? 50
      const PER_NODE_CHARS = 500
      emitAiAction({ kind: 'extract', label: 'AI dom_extract', detail: selector.slice(0, 80) })
      // Build an in-page expression with all params embedded as JSON literals so
      // the selector/attribute names can't break out of the string. evalInPage
      // runs with returnByValue, so we return plain serialisable objects.
      const expr = `(() => {
  const selector = ${JSON.stringify(selector)};
  const fields = ${JSON.stringify(fieldList)};
  const attrs = ${JSON.stringify(attrList)};
  const limit = ${JSON.stringify(cap)};
  const CAP = ${PER_NODE_CHARS};
  const clip = (s) => s.length > CAP ? s.slice(0, CAP) + '…' : s;
  const nodes = Array.from(document.querySelectorAll(selector));
  const matched = nodes.length;
  const items = nodes.slice(0, limit).map((el, index) => {
    const out = { index };
    if (fields.includes('text')) out.text = clip((el.innerText || el.textContent || '').trim());
    if (fields.includes('href')) { const h = el.getAttribute('href'); if (h != null) out.href = el.href || h; }
    if (fields.includes('src')) { const s = el.getAttribute('src'); if (s != null) out.src = el.src || s; }
    if (fields.includes('value') && 'value' in el) out.value = el.value;
    if (fields.includes('html')) out.html = clip(el.innerHTML || '');
    if (attrs.length) {
      const a = {};
      for (const name of attrs) { const v = el.getAttribute(name); if (v != null) a[name] = v; }
      if (Object.keys(a).length) out.attrs = a;
    }
    return out;
  });
  return { matched, count: items.length, truncated: matched > items.length, items };
})()`
      try {
        const result = await evalInPage<{
          matched: number
          count: number
          truncated: boolean
          items: Record<string, unknown>[]
        }>(expr)
        return ok(JSON.stringify(result, null, 2))
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
