import { clipboard } from 'electron'
import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { emitAiAction } from '../../ai-events'
import { scopeBlockForUrl } from '../scope'
import {
  armDialogAnswer,
  getActiveTarget,
  listTargets,
  setActiveTarget,
  waitForSettle
} from '../../chrome-cdp'
import { setViewport } from '../../viewport'
import { evalInPage, visualize } from '../cdp-eval'
import { namedKeys, pressKey } from '../human-input'
import { humanScroll } from '../human-input'
import {
  clickRef,
  clickSelector,
  hoverRef,
  hoverSelector,
  clickRefWith,
  dragRef,
  drawOnRef,
  focusRef,
  scrollRefBy,
  navigateHistory,
  uploadToRef,
  selectRef,
  takeSnapshot,
  typeRef,
  typeSelector
} from '../snapshot'
import { ok, okBudgeted, err, errorMessage } from '../utils'

// One-line preview of an eval result / error for the in-page code HUD.
function preview(text: string): string {
  const flat = text.replace(/\s+/g, ' ').trim()
  return flat.length > 120 ? flat.slice(0, 120) + '…' : flat
}

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
      `${actionResult}\n\n--- snapshot (refs from previous snapshot are now stale) ---\nurl: ${snap.url}\ntitle: ${snap.title}\n\n${snap.tree}${filterNote(snap.stats)}`
    )
  } catch (e) {
    return ok(`${actionResult}\n\n[auto-snapshot failed: ${errorMessage(e)}]`)
  }
}

/** Trailing note telling the agent what the viewport filter left out. */
function filterNote(stats: {
  hidden: number
  offscreen: number
  clickOnlyRefs: number
  clickScanned: number
  clickScanSkipped: boolean
  clickMatched: number
  fellBackToFull: boolean
  frames: number
  framesUnreachable: number
  framesEmpty: number
  newRefs: number
}): string {
  const parts: string[] = []
  if (stats.hidden > 0 || stats.offscreen > 0) {
    parts.push(
      `viewport filter: ${stats.hidden} hidden/covered, ${stats.offscreen} off-screen actionable element(s) omitted — scroll to reach them, or pass full=true for the whole page`
    )
  }
  // Reported even at zero: a silent scan is indistinguishable from a page that
  // genuinely has no role-less click targets, and the scanned/matched split is
  // what separates "scan found nothing" from "geometry failed to correlate".
  if (stats.clickScanSkipped) {
    parts.push(
      'click scan SKIPPED: the page has too many elements, so role-less click targets (a <div> with an onClick and no ARIA role) are missing from this outline entirely'
    )
  }
  if (stats.clickScanned > 0 || stats.clickOnlyRefs > 0) {
    parts.push(
      `click scan: ${stats.clickScanned} candidate(s), ${stats.clickMatched} correlated, ${stats.clickOnlyRefs} new ref(s) tagged (click-scan)`
    )
  }
  if (stats.frames > 0 || stats.framesUnreachable > 0) {
    const bits = [`frames: ${stats.frames} entered`]
    if (stats.framesUnreachable > 0) {
      bits.push(
        `${stats.framesUnreachable} iframe(s) could not be entered (cross-site, runs in its own renderer — not yet supported)`
      )
    }
    if (stats.framesEmpty > 0) {
      bits.push(`${stats.framesEmpty} entered but still empty (frame had not finished loading)`)
    }
    if (stats.frames > 0) {
      bits.push('click scan does not reach inside frames, so framed elements need an ARIA role')
    }
    parts.push(bits.join('; '))
  }
  if (stats.newRefs > 0) {
    parts.push(
      `${stats.newRefs} ref(s) marked *new — absent from the previous snapshot of this page`
    )
  }
  if (stats.fellBackToFull) {
    parts.push(
      'viewport filter DISABLED for this snapshot: it suppressed every element with nothing to attribute it to (likely a coordinate mismatch)'
    )
  }
  return parts.length ? `\n\n[${parts.join(']\n[')}]` : ''
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
      const scopeBlock = scopeBlockForUrl(url)
      if (scopeBlock) return err(scopeBlock)
      const target = getActiveTarget()
      if (!target) return err('no active browser target — open a page first')
      try {
        emitAiAction({ kind: 'navigate', label: `AI navigate`, detail: url })
        // Sweep as soon as the new document is parsed, not after loadURL
        // resolves — loadURL waits for the full load (images, subframes), so
        // sweeping then reads as a beat too late. The overlay script is
        // injected at document-start, so __reverAi already exists here. It has
        // to be the NEW document: the old one's overlay dies on navigation.
        target.wc.once('dom-ready', () => visualize('navSweep'))
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
        'Capture the current page as a compact accessibility-tree snapshot. Returns url, title, and a YAML-like outline where actionable nodes carry [ref=rN] handles. Use these refs in browser_click and browser_type. This is the primary way to "see" the page — far cheaper than dumping HTML or screenshots. By default only what is painted inside the viewport is returned; hidden, overlay-covered and off-screen nodes are omitted and summarised as scroll hints. Scroll (browser_scroll) to reach them, or pass full=true when you genuinely need the whole document.',
      inputSchema: {
        full: z
          .boolean()
          .optional()
          .describe(
            'Return the entire document instead of the viewport-filtered tree (much larger; default false)'
          )
      }
    },
    async ({ full }) => {
      try {
        const snap = await takeSnapshot({ full })
        return okBudgeted(
          `url: ${snap.url}\ntitle: ${snap.title}\n\n${snap.tree}${filterNote(snap.stats)}`
        )
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
    'browser_hover',
    {
      description:
        'Hover over the element identified by ref (from the latest browser_snapshot). Scrolls into view and performs a human-like mouse move WITHOUT clicking — use it to open hover-triggered dropdown menus, tooltips and mega-navs. Returns a fresh snapshot so the hover-revealed content is visible. Refs from the previous snapshot are now stale.',
      inputSchema: {
        ref: z.string().describe('Element ref from browser_snapshot, e.g. "r12"')
      }
    },
    async ({ ref }) => {
      try {
        await hoverRef(ref)
        return await snapshotAfter(`hovered ${ref}`)
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
    'browser_scroll_element',
    {
      description:
        'Scroll a container identified by ref, rather than the window. browser_scroll moves the page, which does nothing for a list, drawer or chat log that scrolls inside its own box — those show as [scrollable +Npx] in the snapshot. Stepped, so lazy-loading handlers fire. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.',
      inputSchema: {
        ref: z.string().describe('Ref of the scroll container, e.g. "r15"'),
        deltaY: z.number().describe('Pixels to scroll; negative scrolls up')
      }
    },
    async ({ ref, deltaY }) => {
      try {
        const top = await scrollRefBy(ref, deltaY)
        return await snapshotAfter(`scrolled ${ref} to ${Math.round(top)}px`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_draw',
    {
      description:
        'Press, move and release inside ONE element — a canvas stroke, a signature pad, a colour picker, a range track. browser_drag goes between two elements and cannot express a gesture that starts and ends on the same surface. Points are fractions of the element box (0..1), so 0.1,0.5 to 0.9,0.5 draws a horizontal line across the middle. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.',
      inputSchema: {
        ref: z.string().describe('Ref of the surface to draw on, e.g. "r24"'),
        from: z
          .object({ x: z.number(), y: z.number() })
          .describe('Start point as fractions of the element box, e.g. {"x":0.1,"y":0.5}'),
        to: z
          .object({ x: z.number(), y: z.number() })
          .describe('End point as fractions of the element box')
      }
    },
    async ({ ref, from, to }) => {
      try {
        await drawOnRef(ref, from, to)
        return await snapshotAfter(`drew on ${ref}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_paste',
    {
      description:
        'Put text on the clipboard and paste it into the element identified by ref. A page that reads clipboardData on a paste event sees nothing when the text is typed instead — the two are different events. Overwrites the real clipboard. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.',
      inputSchema: {
        ref: z.string().describe('Ref of the field to paste into, e.g. "r22"'),
        text: z.string().describe('Text to place on the clipboard and paste')
      }
    },
    async ({ ref, text }) => {
      try {
        clipboard.writeText(text)
        await focusRef(ref)
        emitAiAction({ kind: 'type', label: 'AI paste', detail: text.slice(0, 80) })
        await pressKey('v', { modifiers: [process.platform === 'darwin' ? 'Meta' : 'Control'] })
        return await snapshotAfter(`pasted ${text.length} char(s) into ${ref}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_tabs',
    {
      description:
        'List the open browser tabs, or switch to one. A window.open popup becomes a tab, and its content is unreachable until the tools are pointed at it — every other browser tool acts on the selected tab only. Select by index from the list.',
      inputSchema: {
        action: z.enum(['list', 'select']).describe('What to do'),
        index: z.number().optional().describe('Tab index to select, from the list output')
      }
    },
    async ({ action, index }) => {
      try {
        const tabs = listTargets()
        if (action === 'list') {
          if (!tabs.length) return ok('no attached tabs')
          return ok(
            tabs
              .map((t, i) => `${i}${t.active ? ' *' : '  '} ${t.title || '(untitled)'} — ${t.url}`)
              .join('\n') + '\n\n* = selected'
          )
        }
        if (index == null || index < 0 || index >= tabs.length) {
          return err(`index must be 0..${tabs.length - 1}`)
        }
        if (!setActiveTarget(tabs[index].id)) return err('could not select that tab')
        return await snapshotAfter(`selected tab ${index}: ${tabs[index].url}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_handle_dialog',
    {
      description:
        'Arm the answer the page\'s NEXT confirm() or prompt() will get, then trigger it. Without this every confirm() returns true and every prompt() returns its default, because the app answers dialogs itself so the renderer never blocks — so a "cancel" branch or a typed-in value could not be reached. One-shot: consumed by the first dialog, so a stale answer cannot apply to a later one. Call this BEFORE the click that opens the dialog.',
      inputSchema: {
        accept: z
          .boolean()
          .describe('true for OK, false for Cancel (a cancelled prompt returns null)'),
        promptText: z
          .string()
          .optional()
          .describe('Value a prompt() should return. Ignored when accept is false.')
      }
    },
    async ({ accept, promptText }) => {
      try {
        await armDialogAnswer(accept, promptText)
        const what = accept ? (promptText != null ? `OK with "${promptText}"` : 'OK') : 'Cancel'
        return ok(`next dialog will answer ${what} — now trigger it`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_file_upload',
    {
      description:
        'Attach files to a file input identified by ref. A file input cannot be typed into — the browser owns its value — and clicking it opens a native picker no tool can answer, so this is the only way to fill one. The ref may point at the input or at its rendered button. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.',
      inputSchema: {
        ref: z.string().describe('Ref of the file input, or of its button, e.g. "r7"'),
        paths: z
          .array(z.string())
          .describe('Absolute paths of the files to attach. Pass several only for a multiple input.')
      }
    },
    async ({ ref, paths }) => {
      try {
        const files = await uploadToRef(ref, paths)
        return await snapshotAfter(`attached ${files.length} file(s) to ${ref}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_history',
    {
      description:
        "Move back or forward in the tab's own history. browser_navigate to the previous URL is NOT the same thing: it pushes a new entry, so the page sees a fresh load rather than a restore and anything keyed off popstate or history length behaves differently. Errors when there is nothing to go to. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.",
      inputSchema: {
        direction: z.enum(['back', 'forward']).describe('Which way to move')
      }
    },
    async ({ direction }) => {
      try {
        const url = await navigateHistory(direction)
        await waitForSettle({ idleMs: 300, timeoutMs: 3000 })
        return await snapshotAfter(`went ${direction} to ${url}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_click_modified',
    {
      description:
        'Click a ref with a non-default button or click count: right-click to open a context menu, or double-click. browser_click always sends a single left click, and two browser_click calls are seconds apart so the page never pairs them into a double-click. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.',
      inputSchema: {
        ref: z.string().describe('Element ref from browser_snapshot, e.g. "r12"'),
        button: z
          .enum(['left', 'right', 'middle'])
          .optional()
          .describe('Mouse button (default left)'),
        clickCount: z
          .number()
          .optional()
          .describe('2 for a double-click (default 1)')
      }
    },
    async ({ ref, button, clickCount }) => {
      try {
        await clickRefWith(ref, { button, clickCount })
        const what = clickCount === 2 ? 'double-clicked' : `${button ?? 'left'}-clicked`
        return await snapshotAfter(`${what} ${ref}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_drag',
    {
      description:
        'Drag one ref onto another. Tries the browser\'s own drag machinery first (HTML5 draggable), then falls back to a held-button pointer gesture, which is what dnd-kit / SortableJS listen for. A click on the source followed by a click on the target is two unrelated clicks and neither sees a drag. The result says which mechanism ran — CHECK THE PAGE ACTUALLY CHANGED, a drag that reached nothing looks identical to one that worked. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.',
      inputSchema: {
        from: z.string().describe('Ref of the element to drag, e.g. "r6"'),
        to: z.string().describe('Ref of the drop target, e.g. "r9"')
      }
    },
    async ({ from, to }) => {
      try {
        const how = await dragRef(from, to)
        return await snapshotAfter(`dragged ${from} onto ${to} (${how} drag)`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_press_key',
    {
      description: `Send a single key to the focused element — the only way to reach a keyboard-driven widget. Escape closes a dialog, ArrowDown moves a listbox or a slider, Tab moves focus, and Control+A then Delete clears a field (browser_type appends rather than replacing). Pass ref to focus that element first WITHOUT clicking it, which matters when a click would dismiss or mis-select. Accepts one character, or: ${namedKeys().join(', ')}. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.`,
      inputSchema: {
        key: z.string().describe('Key name (e.g. "Escape", "ArrowDown") or a single character'),
        ref: z
          .string()
          .optional()
          .describe('Focus this element first, without clicking it. Omit to send to whatever has focus.'),
        modifiers: z
          .array(z.enum(['Alt', 'Control', 'Meta', 'Shift']))
          .optional()
          .describe('Held while the key is pressed, e.g. ["Control"] for Control+A'),
        repeat: z.number().optional().describe('Press this many times (default 1)')
      }
    },
    async ({ key, ref, modifiers, repeat }) => {
      try {
        if (ref) await focusRef(ref)
        emitAiAction({
          kind: 'type',
          label: `AI key ${[...(modifiers ?? []), key].join('+')}`,
          detail: ref
        })
        await pressKey(key, { modifiers, repeat })
        const times = repeat && repeat > 1 ? ` x${repeat}` : ''
        return await snapshotAfter(`pressed ${[...(modifiers ?? []), key].join('+')}${times}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'browser_select_option',
    {
      description:
        'Choose option(s) in a <select> identified by ref (from the latest browser_snapshot). A native dropdown is drawn by the OS, so browser_click and browser_type cannot reach its options — this is the only way to answer one. Match by option value or by visible label; if nothing matches, the error lists the available options. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.',
      inputSchema: {
        ref: z.string().describe('Element ref of the <select> from browser_snapshot, e.g. "r20"'),
        values: z
          .array(z.string())
          .describe(
            'Option values or visible labels to select. Pass one for a normal select, several for a multiple one.'
          )
      }
    },
    async ({ ref, values }) => {
      try {
        const picked = await selectRef(ref, values)
        return await snapshotAfter(`selected ${picked.join(', ')} in ${ref}`)
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
    'browser_hover_selector',
    {
      description:
        'Hover over the first element matching a CSS selector, using the SAME human-shaped cursor move as browser_hover (no click). Use when you have a selector but no snapshot ref — NEVER fake hover with raw JS dispatchEvent, as that skips the cursor animation and fires untrusted events. Returns a fresh snapshot.',
      inputSchema: {
        selector: z
          .string()
          .describe('CSS selector for the target, e.g. "nav .menu-item"')
      }
    },
    async ({ selector }) => {
      try {
        await hoverSelector(selector)
        return await snapshotAfter(`hovered selector ${selector}`)
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
        visualize('evalHudStart', expression.slice(0, 160))
        const result = await evalInPage<unknown>(expression)
        const text = typeof result === 'string' ? result : JSON.stringify(result, null, 2)
        visualize('evalHudDone', preview(text), false)
        return ok(text)
      } catch (e) {
        visualize('evalHudDone', preview(errorMessage(e)), true)
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
        visualize('extractHighlight', selector, result.matched)
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
        // Shutter AFTER the capture so the flash never lands in the PNG.
        visualize('shutter')
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
