import { existsSync } from 'node:fs'

import { emitAiAction } from '../ai-events'
import { getActiveTarget, waitForSettle } from '../chrome-cdp'
import { visualize } from './cdp-eval'
import {
  humanDrag,
  humanMouseMove,
  nativeDrag,
  humanPressRelease,
  humanType,
  thinkingPause
} from './human-input'
import { collectFrames } from './frames'
import { capturePageLayout, scanClickable, type PageLayout } from './layout'

interface AXValue {
  type: string
  value?: unknown
}

interface AXProperty {
  name: string
  value: AXValue
}

export interface AXNode {
  nodeId: string
  ignored?: boolean
  childIds?: string[]
  role?: AXValue
  name?: AXValue
  value?: AXValue
  properties?: AXProperty[]
  backendDOMNodeId?: number
}

/** An `<iframe>` element on the path from the page to a node, outermost first. */
interface FrameOwner {
  backendNodeId: number
  /** Session of the document CONTAINING this `<iframe>`, not of its content. */
  sessionId?: string
}

interface RefEntry {
  backendNodeId: number
  role: string
  name: string
  /**
   * The `<iframe>` chain this node sits behind, outermost first.
   *
   * Coordinates from getBoundingClientRect inside a frame are relative to THAT
   * frame, while mouse events are dispatched in page space, so the frames'
   * positions have to be added back. They are measured at click time rather
   * than stored from the snapshot: the click path calls scrollIntoView first,
   * which moves the page under the frame, and a stored offset is wrong by
   * exactly that scroll. The symptom is not an error — the click lands on
   * whatever is now at those coordinates. It selected the wrong card issuer
   * and silently unticked a consent box before this was measured live.
   */
  frameOwners?: FrameOwner[]
  /**
   * CDP session owning this node. Absent for the page session. Every command
   * about the node must carry it: backend ids are per-process, so the page
   * session answers about an out-of-process node with a different node rather
   * than an error.
   */
  sessionId?: string
}

const refMap = new Map<string, RefEntry>()

/**
 * Reverse lookup: which ref from the CURRENT snapshot points at this backend
 * node, if any. Backend ids are per-process, so the sessionId must match too —
 * undefined means the page session (see RefEntry.sessionId).
 */
export function refForBackendNodeId(backendNodeId: number, sessionId?: string): string | null {
  for (const [ref, entry] of refMap) {
    if (entry.backendNodeId === backendNodeId && entry.sessionId === sessionId) return ref
  }
  return null
}

/**
 * Nodes that carried a ref in the previous snapshot, so the next one can point
 * at what just appeared. Keyed by session + backendNodeId, because the ids of
 * an out-of-process frame collide with the page's own.
 *
 * In browser-use the equivalent marker is an attention aid. Here it is closer
 * to evidence: paired with the traffic store, "what showed up right after this
 * request" is how a response field gets traced to the UI that renders it.
 */
let previousRefTargets = new Set<string>()
let previousUrl = ''
/**
 * Which filtering mode produced the baseline. A viewport-filtered snapshot and
 * a full one contain different node sets, so comparing across them reports
 * everything the filter had hidden as newly appeared — the first run of this
 * marked an off-screen button as new when nothing had changed.
 */
let previousPruned = true

const SKIP_ROLES = new Set([
  'none',
  'generic',
  'InlineTextBox',
  'LineBreak',
  'presentation',
  'LayoutTable',
  'LayoutTableRow',
  'LayoutTableCell',
  'LayoutTableColumn'
])

/**
 * Roles whose subtree is label, not structure. A button or link already prints
 * its accessible name, so everything under it repeats information the agent
 * has - except a control nested inside, which keeps its own ref and survives.
 */
const LABEL_ONLY_SUBTREE = new Set(['button', 'link', 'menuitem', 'tab', 'option', 'switch'])

const ACTIONABLE_ROLES = new Set([
  'button',
  'link',
  'textbox',
  'searchbox',
  'combobox',
  'checkbox',
  'radio',
  'menuitem',
  'tab',
  'switch',
  'slider',
  'option',
  // A `<select multiple>` renders as a listbox whose options carry refs while
  // the element itself carried none — leaving browser_select_option with
  // nothing to address, so a multi-select could not be answered at all.
  'listbox'
])

/**
 * Bring `this` into view before measuring it. Interpolated into the
 * callFunctionOn bodies of the click / hover / type / drag paths.
 *
 * Two things it is careful about. It only scrolls when the element is actually
 * outside the viewport — centring on every click made the page jump on targets
 * that were already visible. And when it does scroll it animates, then waits
 * for the position to stop moving, because the caller measures coordinates
 * straight afterwards and a still-animating page yields a stale point.
 *
 * The rAF wait is raced against a timer: a backgrounded or occluded window
 * never produces an animation frame, and that is the normal state when an
 * agent drives the app without watching it.
 */
const SCROLL_INTO_VIEW = `
      const r0 = this.getBoundingClientRect()
      const margin = 8
      const offscreen =
        r0.top < margin || r0.left < margin ||
        r0.bottom > innerHeight - margin || r0.right > innerWidth - margin
      if (offscreen) {
        this.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'smooth' })
        let last = NaN
        let settled = 0
        for (let i = 0; i < 40 && settled < 3; i++) {
          await new Promise(r => setTimeout(r, 25))
          const top = this.getBoundingClientRect().top
          settled = Math.abs(top - last) < 0.5 ? settled + 1 : 0
          last = top
        }
      } else {
        await Promise.race([
          new Promise(r => requestAnimationFrame(() => r())),
          new Promise(r => setTimeout(r, 120))
        ])
      }`

/** Depth-limited: labels come from a node's own text, not a whole subtree dump. */
const TEXT_LABEL_DEPTH = 3

/** First piece of visible text under a node, used to name role-less click targets. */
export function firstText(node: AXNode, scope: Map<string, AXNode>, depth = 0): string {
  if (depth > TEXT_LABEL_DEPTH) return ''
  for (const id of node.childIds ?? []) {
    const child = scope.get(id)
    if (!child) continue
    const name = String((child.name?.value as string | undefined) ?? '').trim()
    if ((child.role?.value as string | undefined) === 'StaticText' && name) return name
    const nested = firstText(child, scope, depth + 1)
    if (nested) return nested
  }
  return ''
}

function quote(s: string): string {
  return JSON.stringify(s.length > 80 ? s.slice(0, 80) + '…' : s)
}

export interface SnapshotStats {
  /** Actionable nodes that got a ref. */
  refs: number
  /** Nodes dropped because nothing is painted for them (hidden or covered). */
  hidden: number
  /** Nodes dropped because their box sits outside the viewport. */
  offscreen: number
  /** Refs that exist only because the click scan found them (no ARIA role). */
  clickOnlyRefs: number
  /** Elements the in-page click scan reported, before geometry correlation. */
  clickScanned: number
  /**
   * True when the page was too large and the scan never ran. Every click-only
   * ref disappears with it, so the outline silently loses `<div onClick>`
   * targets — an infinite-scroll list growing past the cap did exactly that
   * mid-session, and nothing said so.
   */
  clickScanSkipped: boolean
  /** How many of those correlated to a snapshot node. */
  clickMatched: number
  /** True when the filter was discarded because it suppressed everything. */
  fellBackToFull: boolean
  /** Child frames whose content was spliced into the tree. */
  frames: number
  /**
   * `<iframe>` elements present in the outline that could NOT be entered.
   *
   * Counted from the tree, not from Page.getFrameTree: an out-of-process frame
   * belongs to another target and never appears in the frame tree at all, so
   * counting failures there reported 0 while a whole frame was missing.
   */
  framesUnreachable: number
  /** Frames entered but still empty after a retry (likely still loading). */
  framesEmpty: number
  /** Refs marked *new because they were absent from the previous snapshot. */
  newRefs: number
}

export interface SnapshotResult {
  url: string
  title: string
  tree: string
  stats: SnapshotStats
}

interface PageMeta {
  url: string
  title: string
  scrollX: number
  scrollY: number
  innerWidth: number
  innerHeight: number
}

/** Tally of actionable nodes the viewport filter kept out of the tree. */
export interface FilterTally {
  hidden: number
  below: number
  above: number
  side: number
  nearestBelow: number
  nearestAbove: number
}

/**
 * Count what the filter excludes, by scanning every AX node directly.
 *
 * This deliberately does NOT reuse the tree walk. The walk prunes whole
 * subtrees when a container falls off-screen, so an actionable node buried
 * under one is never visited — counting there under-reports by an order of
 * magnitude and suppresses the scroll hints that make pruning safe.
 */
export function tallyFiltered(nodes: AXNode[], layout: PageLayout): FilterTally {
  const t: FilterTally = {
    hidden: 0,
    below: 0,
    above: 0,
    side: 0,
    nearestBelow: Infinity,
    nearestAbove: Infinity
  }
  const v = layout.viewport

  for (const n of nodes) {
    if (n.backendDOMNodeId == null || n.ignored) continue
    const role = (n.role?.value as string | undefined) ?? ''
    if (!ACTIONABLE_ROLES.has(role)) continue

    const lay = layout.byBackendId.get(n.backendDOMNodeId)
    if (!lay) continue

    if (!lay.rendered || lay.occluded || lay.zeroSize) {
      t.hidden++
      continue
    }
    if (lay.inViewport) continue

    const below = lay.y - (v.y + v.height)
    const above = v.y - (lay.y + lay.height)
    if (below > 0) {
      t.below++
      t.nearestBelow = Math.min(t.nearestBelow, below)
    } else if (above > 0) {
      t.above++
      t.nearestAbove = Math.min(t.nearestAbove, above)
    } else {
      t.side++
    }
  }

  return t
}

export function describeOffscreen(t: FilterTally): string[] {
  const out: string[] = []
  // The distance is to the NEAREST element, not to all of them — say so, or a
  // "scroll down ~25px" hint alongside a count of 138 reads as "25px reveals
  // everything" and the agent scrolls one row at a time.
  if (t.below > 0) {
    const px = Math.max(0, Math.round(t.nearestBelow))
    out.push(`- [${t.below} more actionable element(s) below the fold — nearest ~${px}px down]`)
  }
  if (t.above > 0) {
    const px = Math.max(0, Math.round(t.nearestAbove))
    out.push(`- [${t.above} more actionable element(s) above the fold — nearest ~${px}px up]`)
  }
  if (t.side > 0) {
    out.push(`- [${t.side} more actionable element(s) outside the horizontal viewport]`)
  }
  return out
}

/**
 * Capture the page as an accessibility outline.
 *
 * By default only what is actually painted inside the viewport is emitted:
 * hidden, zero-size, overlay-covered and off-screen subtrees are dropped, and
 * off-screen actionable nodes are summarised as scroll hints instead. That
 * keeps the tree small enough that the agent keeps using refs rather than
 * falling back to raw JS (which skips the human-shaped input path and fires
 * untrusted events). Pass `full` to get the unfiltered tree back.
 */
export async function takeSnapshot(opts: { full?: boolean } = {}): Promise<SnapshotResult> {
  const first = await captureSnapshot(opts)

  // "Actionable elements exist, none of them are visible" is what an unsettled
  // layout looks like: the boxes have been created but not yet placed, so they
  // all read as sitting below the fold. It is also indistinguishable from a
  // correct snapshot of a page whose controls really are all off-screen, which
  // is why this waits and re-reads rather than trying to decide.
  //
  // A cold load of a heavy page produced exactly this - zero refs, while the
  // very next snapshot found 25. The agent's first look at a page is the one
  // that shapes the rest of the session, so an empty first look is expensive.
  const looksUnsettled =
    !opts.full &&
    first.stats.refs === 0 &&
    first.stats.hidden + first.stats.offscreen > 0

  if (!looksUnsettled) return first

  // Wait the way the page needs rather than a fixed pause: a cold load can
  // still be fetching well past the normal settle window, and a flat delay is
  // either too short for that or wasted on every other page.
  await waitForSettle({ idleMs: 300, timeoutMs: 2500, minWaitMs: 300 })
  const second = await captureSnapshot(opts)
  return second.stats.refs > 0 ? second : first
}

async function captureSnapshot(opts: { full?: boolean }): Promise<SnapshotResult> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target — open a page first')
  // Emit here (not per-tool) so implicit snapshots after navigate/click/type
  // also surface in the AI-activity overlay. The in-page scan runs alongside
  // the AX tree walk (not awaited) so it never slows the agent down.
  emitAiAction({ kind: 'snapshot', label: 'AI snapshot' })
  // The boxes are drawn once the refs exist (below), not here — the animation
  // should show what the agent can actually address.

  await target.dbg.sendCommand('Accessibility.enable').catch(() => {})

  const metaRes = (await target.dbg.sendCommand('Runtime.evaluate', {
    expression:
      '({ url: location.href, title: document.title, scrollX, scrollY, innerWidth, innerHeight })',
    returnByValue: true
  })) as { result: { value: PageMeta } }
  const meta = metaRes.result.value

  const viewport = {
    x: meta.scrollX,
    y: meta.scrollY,
    width: meta.innerWidth,
    height: meta.innerHeight
  }

  // DOMSnapshot bounds come back in the layout (device-pixel) space, while the
  // viewport above is CSS px. On a display whose scale factor is not 1 — the
  // default on Retina, and anything the user picks in "scaled resolution" — the
  // two spaces disagree and every box lands off-screen, so the viewport filter
  // drops the whole tree. getLayoutMetrics reports both spaces; their ratio is
  // the exact factor to divide the snapshot bounds by. Reads 1 (no-op) when the
  // scale is 1, so the common case is untouched.
  let layoutScale = 1
  try {
    const m = (await target.dbg.sendCommand('Page.getLayoutMetrics')) as {
      layoutViewport?: { clientWidth?: number }
      cssLayoutViewport?: { clientWidth?: number }
    }
    const dev = m.layoutViewport?.clientWidth ?? 0
    const css = m.cssLayoutViewport?.clientWidth ?? 0
    if (dev > 0 && css > 0) {
      const s = dev / css
      if (s > 1.02 || s < 0.98) layoutScale = s
    }
  } catch {
    // Older Chromium without cssLayoutViewport — leave scale at 1.
  }

  // The AX tree, the click scan and the layout snapshot are independent reads —
  // issuing them together keeps the added passes off the critical path.
  // The click scan feeds the layout pass, so those two are a chain; the AX tree
  // is independent and runs alongside the whole chain.
  //
  // `full` only turns off viewport PRUNING — the click scan still runs. The
  // agent reaches for full=true when it cannot find something, so having that
  // switch also drop the click-scan refs would remove exactly the elements it
  // is looking for.
  let clickScanSkipped = false
  const [axRes, layout, frameRes] = await Promise.all([
    target.dbg.sendCommand('Accessibility.getFullAXTree') as Promise<{ nodes: AXNode[] }>,
    scanClickable()
      .catch(() => null)
      .then((click) => {
        clickScanSkipped = click?.skipped === true
        return capturePageLayout(viewport, click?.paths, layoutScale).catch(() => null)
      }),
    collectFrames().catch(() => ({ frames: [], unreachable: 0, empty: 0 }))
  ])
  const { nodes } = axRes
  // Keyed by session + backend id. Backend ids are per-process, so a frame
  // nested inside an out-of-process one can carry the same number as an
  // unrelated element in the page — keying on the number alone would splice a
  // frame into whatever happened to share it.
  const framesByOwner = new Map(
    frameRes.frames.map((f) => [`${f.ownerSessionId ?? ''}:${f.ownerBackendNodeId}`, f])
  )

  // After a navigation every node is new, so the marker would say nothing. Only
  // compare within the same document.
  const sameDocument =
    previousUrl === meta.url && previousRefTargets.size > 0 && previousPruned === !opts.full

  const byId = new Map<string, AXNode>()
  for (const n of nodes) byId.set(n.nodeId, n)

  /**
   * The full `<iframe>` chain from the page down to `frame`, outermost first.
   *
   * The walk builds this as it descends, but a frame with no anchor is emitted
   * on its own, and its immediate owner may itself sit inside another frame.
   * Following ownerSessionId back up recovers the missing outer hops — without
   * them a click is short by the outer frame's position and lands elsewhere
   * without erroring.
   */
  const ownerChain = (frame: (typeof frameRes.frames)[number]): FrameOwner[] => {
    const chain: FrameOwner[] = []
    let cur: (typeof frameRes.frames)[number] | undefined = frame
    const guard = new Set<string>()
    while (cur && !guard.has(cur.frameId)) {
      guard.add(cur.frameId)
      chain.unshift({ backendNodeId: cur.ownerBackendNodeId, sessionId: cur.ownerSessionId })
      const parentSession: string | undefined = cur.ownerSessionId
      cur = parentSession
        ? frameRes.frames.find((f) => f.sessionId === parentSession)
        : undefined
    }
    return chain
  }

  const build = (
    filter: PageLayout | null,
    prune: boolean
  ): { tree: string; stats: SnapshotStats; refTargets: Set<string> } => {
    refMap.clear()
    let counter = 0
    let clickOnlyRefs = 0
    let newRefs = 0
    const seenRefTargets = new Set<string>()
    let framesEntered = 0
    let framesMissed = 0
    const splicedFrames = new Set<string>()
    const lines: string[] = []

    const walk = (
      id: string,
      depth: number,
      parentName: string,
      ancestorClaimed: boolean,
      scope: Map<string, AXNode>,
      frameOwners: FrameOwner[] = [],
      ancestorControl = false,
      frameSessionId?: string
    ): void => {
      const n = scope.get(id)
      if (!n) return
      const role = (n.role?.value as string | undefined) ?? ''
      const name = String((n.name?.value as string | undefined) ?? '').trim()

      // Layout gate. A node with no layout entry is left alone — absence of
      // data must never be read as "invisible", or whole pages would vanish.
      // Counting happens in tallyFiltered, not here: this prunes subtrees, so
      // anything below a pruned container is never reached.
      //
      // Skipped entirely inside an out-of-process frame: the layout pass runs
      // in the page's renderer, and that frame's backend ids belong to another
      // process. A lookup there does not miss, it MATCHES A DIFFERENT ELEMENT —
      // so the filter would prune or keep the frame's nodes at random.
      const lay =
        filter && !frameSessionId && n.backendDOMNodeId != null
          ? filter.byBackendId.get(n.backendDOMNodeId)
          : undefined
      // A zero-size box is invisible on its own but its children can overflow
      // it, so it must not take the subtree down with it — only style-hidden,
      // covered and off-screen boxes do.
      if (prune && lay && !lay.zeroSize && (!lay.rendered || lay.occluded || !lay.inViewport)) {
        return
      }

      // A click signal with no interactive ARIA role — the `<div onClick>`
      // case. Only the outermost such node in a chain gets a ref, so a button
      // wrapped in three pointer-cursor divs still yields exactly one.
      const claimable =
        lay?.clickable === true &&
        !ancestorClaimed &&
        !ACTIONABLE_ROLES.has(role) &&
        n.backendDOMNodeId != null

      // StaticText is collapsed into its parent's name. Suppress entirely if it
      // duplicates the parent name (Playwright MCP rule); otherwise emit as a
      // single quoted text line without a ref (text is not actionable).
      if (role === 'StaticText' && !claimable) {
        if (ancestorControl) return
        if (!name || name === parentName) return
        lines.push(`${'  '.repeat(depth)}- text ${quote(name)}`)
        return
      }

      // Inside a link or button, the label text and its wrapper spans carry no
      // information the ancestor line does not already give — a button prints
      // its own accessible name. Emitting them is most of what the outline
      // wastes on a real page. Anything with its own ref survives, so a nested
      // control or a click-scan hit is never swallowed.
      const insideControl = ancestorControl && !claimable && !ACTIONABLE_ROLES.has(role)

      const isScroller = (lay?.scrollableBy ?? 0) > 0
      const skip =
        !isScroller &&
        (insideControl || (!claimable && (!role || n.ignored || (SKIP_ROLES.has(role) && !name))))
      let nextDepth = depth

      if (!skip) {
        // A click-scan node has no ARIA role or name by definition, so it would
        // print as a bare `- generic [ref=r4]`. Label it by its own text so the
        // agent can tell what it is about to click.
        const label = claimable && (!role || SKIP_ROLES.has(role)) ? 'clickable' : role
        const shownName = name || (claimable ? firstText(n, scope) : '')

        const parts: string[] = [`- ${label || (isScroller ? 'scroller' : 'generic')}`]
        if (shownName) parts.push(quote(shownName))

        if (n.value?.value !== undefined && n.value.value !== '') {
          parts.push(`value=${quote(String(n.value.value))}`)
        }

        if (n.properties) {
          for (const p of n.properties) {
            const v = p.value?.value
            if ((p.name === 'checked' || p.name === 'selected' || p.name === 'expanded') && v) {
              parts.push(`${p.name}=${v}`)
            }
            if (p.name === 'disabled' && v) parts.push('disabled')
            if (p.name === 'level' && typeof v === 'number') parts.push(`level=${v}`)
          }
        }

        // A scroll container gets a ref too. browser_scroll moves the window,
        // so a list that appends as its OWN box scrolls could be seen but never
        // advanced — the outline said `[scrollable +132px]` with nothing to
        // address. Same for a canvas: the click scan finds it, but a stroke
        // needs the element itself as the drag surface.
        if (n.backendDOMNodeId != null && (ACTIONABLE_ROLES.has(role) || claimable || isScroller)) {
          counter++
          if (claimable) clickOnlyRefs++
          const refKey = `${frameSessionId ?? ''}:${n.backendDOMNodeId}`
          if (sameDocument && !previousRefTargets.has(refKey)) {
            parts.push('*new')
            newRefs++
          }
          seenRefTargets.add(refKey)
          const r = `r${counter}`
          refMap.set(r, {
            backendNodeId: n.backendDOMNodeId,
            role: role || 'clickable',
            name: shownName,
            frameOwners: frameOwners.length ? frameOwners : undefined,
            sessionId: frameSessionId
          })
          parts.push(`[ref=${r}]`)
          if (claimable) parts.push('(click-scan)')
        }

        // A scrollable box is worth calling out even when it carries no ref:
        // browser_scroll moves the window, so if the real scroller is this
        // container the agent needs to know it exists to reach what is inside.
        if (lay && lay.scrollableBy > 0) parts.push(`[scrollable +${lay.scrollableBy}px]`)

        lines.push(`${'  '.repeat(depth)}${parts.join(' ')}`)
        nextDepth = depth + 1
      }

      const childParentName = skip ? parentName : name
      const nextAncestorControl = ancestorControl || LABEL_ONLY_SUBTREE.has(role)
      for (const c of n.childIds ?? []) {
        walk(
          c,
          nextDepth,
          childParentName,
          ancestorClaimed || claimable,
          scope,
          frameOwners,
          nextAncestorControl,
          frameSessionId
        )
      }

      // Descend into a child frame. Its accessibility tree is a separate
      // document with its own node ids, so the scope map switches here rather
      // than being merged — ids collide across frames.
      //
      // Looked up per session, so a frame nested inside an out-of-process one
      // is reached from that frame's own document rather than being matched by
      // a bare backend id that means something else in the page.
      const frame =
        n.backendDOMNodeId != null
          ? framesByOwner.get(`${frameSessionId ?? ''}:${n.backendDOMNodeId}`)
          : undefined
      if (!frame && role === 'Iframe') framesMissed++
      if (frame) {
        framesEntered++
        splicedFrames.add(frame.frameId)
        // Record the `<iframe>` rather than its position — the position is
        // measured at click time, see RefEntry.frameOwners.
        walk(
          frame.rootNodeId,
          nextDepth,
          name,
          ancestorClaimed || claimable,
          frame.byId,
          [...frameOwners, { backendNodeId: n.backendDOMNodeId as number, sessionId: frameSessionId }],
          false,
          frame.sessionId
        )
      }
    }

    if (nodes[0]) walk(nodes[0].nodeId, 0, '', false, byId)

    // Elements the click scan found that the accessibility tree has no node
    // for at all. A `<canvas>` with no accessible content is the common case —
    // charts, signature pads, maps and games are all reachable by mouse and
    // completely absent from the outline, so there was nothing to address.
    // Emitted after the walk with their own refs.
    if (filter) {
      for (const [backendId, lay] of filter.byBackendId) {
        if (!lay.clickable) continue
        if (seenRefTargets.has(`:${backendId}`)) continue
        if (prune && (!lay.rendered || lay.occluded || !lay.inViewport)) continue
        if (lay.width < 8 || lay.height < 8) continue
        counter++
        clickOnlyRefs++
        const r = `r${counter}`
        seenRefTargets.add(`:${backendId}`)
        refMap.set(r, { backendNodeId: backendId, role: 'clickable', name: '' })
        lines.push(`- clickable [ref=${r}] (click-scan, not in the a11y tree)`)
      }
    }

    // A frame whose `<iframe>` carries aria-hidden has NO node in the parent
    // accessibility tree — Chrome drops the element outright rather than
    // marking it ignored — so the walk never reaches an anchor to nest it
    // under. Emitting it at the end keeps it reachable instead of dropping it
    // without a word: a cross-site frame the page hides from assistive tech
    // (trackers, 3DS steps, captchas, status embeds) is exactly what a
    // reverser is looking for, and on the Toss sandbox this was the only real
    // OOPIF on the page.
    for (const frame of frameRes.frames) {
      if (splicedFrames.has(frame.frameId)) continue
      framesEntered++
      lines.push(`- Iframe (hidden from the a11y tree) ${quote(frame.url ?? frame.frameId)}`)
      walk(
        frame.rootNodeId,
        1,
        '',
        false,
        frame.byId,
        ownerChain(frame),
        false,
        frame.sessionId
      )
    }

    if (!filter || !prune) {
      return {
        tree: lines.join('\n'),
        refTargets: seenRefTargets,
        stats: {
          refs: counter,
          hidden: 0,
          offscreen: 0,
          clickOnlyRefs,
          clickScanned: filter?.clickScanned ?? 0,
          clickScanSkipped,
          clickMatched: filter?.clickMatched ?? 0,
          fellBackToFull: false,
          frames: framesEntered,
          framesUnreachable: framesMissed,
          framesEmpty: frameRes.empty,
          newRefs
        }
      }
    }

    const tally = tallyFiltered(nodes, filter)
    return {
      tree: [...lines, ...describeOffscreen(tally)].join('\n'),
      refTargets: seenRefTargets,
      stats: {
        refs: counter,
        hidden: tally.hidden,
        offscreen: tally.below + tally.above + tally.side,
        clickOnlyRefs,
        clickScanned: filter.clickScanned,
        clickScanSkipped,
        clickMatched: filter.clickMatched,
        fellBackToFull: false,
        frames: framesEntered,
        framesUnreachable: framesMissed,
        framesEmpty: frameRes.empty,
        newRefs
      }
    }
  }

  let result = build(layout, !opts.full)

  // Safety valve for a coordinate mismatch (detached document, odd frame
  // setup): the filter suppressed everything AND cannot say why.
  //
  // "Cannot say why" is the load-bearing half. A full-screen modal legitimately
  // covers every actionable element, which also yields zero refs — firing the
  // valve there throws away a correct result and silently disables the filter
  // exactly when the page is most confusing. So only fall back when the tally
  // accounts for nothing.
  if (layout && !opts.full && result.stats.refs === 0) {
    const explained = result.stats.hidden > 0 || result.stats.offscreen > 0
    if (!explained) {
      result = build(layout, false)
      // Only a real mismatch if dropping the filter actually reveals controls.
      // An error page with no controls at all also lands here, and claiming a
      // coordinate problem there is a false alarm.
      result.stats.fellBackToFull = result.stats.refs > 0
    }
  }

  previousRefTargets = result.refTargets
  previousUrl = meta.url
  previousPruned = !opts.full

  // Outline exactly the elements that got a ref, tagged with it. Reuses the
  // layout pass already taken, so this costs one extra evaluate rather than a
  // measurement per element. Nodes inside a frame are skipped: the layout runs
  // in the page's renderer, and a framed node's box is either absent from it or
  // expressed in another coordinate space.
  if (layout) {
    const boxes: Array<{ ref: string; x: number; y: number; w: number; h: number }> = []
    for (const [ref, entry] of refMap) {
      if (entry.frameOwners?.length) continue
      const lay = layout.byBackendId.get(entry.backendNodeId)
      if (!lay) continue
      boxes.push({
        ref,
        x: lay.x - viewport.x,
        y: lay.y - viewport.y,
        w: lay.width,
        h: lay.height
      })
    }
    visualize('scanPage', boxes)
  }

  return {
    url: meta.url,
    title: meta.title,
    tree: result.tree,
    stats: result.stats
  }
}

/**
 * A ref's live RemoteObject plus the CDP session it lives in — follow-up
 * commands about the node (or objects derived from it) must carry the same
 * session, see RefEntry.sessionId.
 */
export async function resolveRefObject(
  ref: string
): Promise<{ objectId: string; sessionId: string | undefined }> {
  return { objectId: await resolveObjectId(ref), sessionId: refMap.get(ref)?.sessionId }
}

async function resolveObjectId(ref: string): Promise<string> {
  const entry = refMap.get(ref)
  if (!entry) throw new Error(`unknown ref "${ref}" — call browser_snapshot first`)
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')
  // The session is load-bearing, not an optimisation: asked about an
  // out-of-process node without it, the page session returns a DIFFERENT node
  // instead of failing, and the click lands somewhere else entirely.
  const res = (await target.dbg.sendCommand(
    'DOM.resolveNode',
    { backendNodeId: entry.backendNodeId },
    entry.sessionId
  )) as { object: { objectId: string } }
  return res.object.objectId
}

/**
 * Resolve a CSS selector to a live RemoteObject id for its first match. Throws
 * if nothing matches. This lets selector-based tools reuse the exact same
 * human-input path (cursor move + flash + real events) as ref-based ones
 * WITHOUT needing a prior browser_snapshot — the fix for pages whose snapshot
 * is too big to be practical (the agent used to fall back to raw JS, which
 * skips the whole animation and fires no trusted events).
 */
export async function resolveSelectorObjectId(selector: string): Promise<string> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')
  const res = (await target.dbg.sendCommand('Runtime.evaluate', {
    expression: `document.querySelector(${JSON.stringify(selector)})`,
    returnByValue: false
  })) as { result: { objectId?: string } }
  if (!res.result.objectId) {
    throw new Error(`no element matches selector: ${selector}`)
  }
  return res.result.objectId
}

/**
 * Current page-space position of a frame's content origin, measured now.
 *
 * Walks the `<iframe>` chain outermost-in, each element read in the session of
 * the document that holds it, and adds the border + padding because the child
 * document starts at the content box, not the border box. Called after
 * scrollIntoView so the numbers describe where the frame actually is at the
 * moment the mouse event is dispatched.
 */
async function measureFrameOffset(owners: FrameOwner[] | undefined): Promise<{
  x: number
  y: number
}> {
  if (!owners?.length) return { x: 0, y: 0 }
  const target = getActiveTarget()
  if (!target) return { x: 0, y: 0 }

  let x = 0
  let y = 0
  for (const owner of owners) {
    const node = (await target.dbg.sendCommand(
      'DOM.resolveNode',
      { backendNodeId: owner.backendNodeId },
      owner.sessionId
    )) as { object: { objectId: string } }
    const res = (await target.dbg.sendCommand(
      'Runtime.callFunctionOn',
      {
        objectId: node.object.objectId,
        functionDeclaration: `function() {
          const r = this.getBoundingClientRect()
          const s = getComputedStyle(this)
          const n = (v) => parseFloat(v) || 0
          return {
            x: r.left + n(s.borderLeftWidth) + n(s.paddingLeft),
            y: r.top + n(s.borderTopWidth) + n(s.paddingTop)
          }
        }`,
        returnByValue: true
      },
      owner.sessionId
    )) as { result: { value: { x: number; y: number } } }
    x += res.result.value.x
    y += res.result.value.y
  }
  return { x, y }
}

/**
 * Core click: given an already-resolved objectId + a human-readable label,
 * run the full scroll → thinking pause → cursor move → flash → press/release
 * sequence. The overlay flash appears only once the cursor has arrived. Shared
 * by clickRef (snapshot ref) and clickSelector (CSS selector).
 */
async function clickObjectId(
  objectId: string,
  label: string,
  role?: string,
  frameOwners?: FrameOwner[],
  sessionId?: string,
  mouse: { button?: 'left' | 'right' | 'middle'; clickCount?: number } = {}
): Promise<void> {
  const target = getActiveTarget()!
  emitAiAction({ kind: 'click', label, detail: role })

  const result = (await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: `async function() {
      ${SCROLL_INTO_VIEW}
      const r = this.getBoundingClientRect()
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 }
    }`,
    returnByValue: true,
    awaitPromise: true
  }, sessionId)) as { result: { value: { x: number; y: number } } }
  // getBoundingClientRect ran inside the element's own frame, so its origin is
  // that frame — mouse events are dispatched in page space. Measured after the
  // scrollIntoView above, which moves the page under the frame.
  const frameOffset = await measureFrameOffset(frameOwners)
  const x = result.result.value.x + frameOffset.x
  const y = result.result.value.y + frameOffset.y

  await thinkingPause()
  await humanMouseMove(x, y)

  await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: `function(label) {
      if (window.__reverAi) window.__reverAi.flashElement(this, label, 'click')
    }`,
    arguments: [{ value: label }]
  }, sessionId)
  await humanPressRelease(x, y, mouse)
}

/**
 * Page-space centre of a ref, after scrolling it into view. Shared by the
 * gesture entry points so they inherit the frame-offset handling rather than
 * re-deriving coordinates that would be wrong inside an iframe.
 */
async function pointForRef(ref: string): Promise<{ x: number; y: number }> {
  const entry = refMap.get(ref)
  const objectId = await resolveObjectId(ref)
  const target = getActiveTarget()!
  const res = (await target.dbg.sendCommand(
    'Runtime.callFunctionOn',
    {
      objectId,
      functionDeclaration: `async function() {
        ${SCROLL_INTO_VIEW}
        const r = this.getBoundingClientRect()
        return { x: r.left + r.width / 2, y: r.top + r.height / 2 }
      }`,
      returnByValue: true,
      awaitPromise: true
    },
    entry?.sessionId
  )) as { result: { value: { x: number; y: number } } }
  const off = await measureFrameOffset(entry?.frameOwners)
  return { x: res.result.value.x + off.x, y: res.result.value.y + off.y }
}

/** Right-click, middle-click or double-click a ref. */
export async function clickRefWith(
  ref: string,
  mouse: { button?: 'left' | 'right' | 'middle'; clickCount?: number }
): Promise<void> {
  const entry = refMap.get(ref)
  const objectId = await resolveObjectId(ref)
  const kind = mouse.clickCount === 2 ? 'double-click' : `${mouse.button ?? 'left'}-click`
  const label = `AI ${kind}${entry?.name ? ` "${entry.name.slice(0, 28)}"` : ''}`
  await clickObjectId(objectId, label, entry?.role, entry?.frameOwners, entry?.sessionId, mouse)
}

/**
 * A cheap fingerprint of the drop target, used to tell whether a drag that
 * dispatched cleanly actually changed anything. A gesture no library listened
 * to leaves this identical.
 */
async function dropSignature(ref: string): Promise<string> {
  const entry = refMap.get(ref)
  const objectId = await resolveObjectId(ref)
  const target = getActiveTarget()!
  const res = (await target.dbg.sendCommand(
    'Runtime.callFunctionOn',
    {
      objectId,
      functionDeclaration:
        'function() { return this.className + "|" + this.childElementCount + "|" + (this.textContent || "").slice(0, 200) }',
      returnByValue: true
    },
    entry?.sessionId
  )) as { result: { value: string } }
  return res.result.value
}

/**
 * Dispatch a full HTML5 drag sequence with a shared DataTransfer.
 *
 * Last resort, and deliberately explicit about it: the handlers run and the
 * DataTransfer carries whatever dragstart put in it, but the events are not
 * trusted input, so a page that checks isTrusted will not be fooled.
 */
async function synthesiseDrag(fromRef: string, toRef: string): Promise<void> {
  const fromEntry = refMap.get(fromRef)
  const fromObj = await resolveObjectId(fromRef)
  const toObj = await resolveObjectId(toRef)
  const target = getActiveTarget()!

  await target.dbg.sendCommand(
    'Runtime.callFunctionOn',
    {
      objectId: fromObj,
      functionDeclaration: `function(drop) {
        const dt = new DataTransfer()
        const at = (el) => {
          const r = el.getBoundingClientRect()
          return { clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 }
        }
        const fire = (el, type, pos) => {
          const ev = new DragEvent(type, { bubbles: true, cancelable: true, dataTransfer: dt, ...pos })
          el.dispatchEvent(ev)
          return ev
        }
        fire(this, 'dragstart', at(this))
        fire(drop, 'dragenter', at(drop))
        fire(drop, 'dragover', at(drop))
        fire(drop, 'drop', at(drop))
        fire(this, 'dragend', at(drop))
      }`,
      arguments: [{ objectId: toObj }]
    },
    fromEntry?.sessionId
  )
}

/**
 * Scroll a specific container rather than the window.
 *
 * `browser_scroll` moves the page, which does nothing for a list that scrolls
 * inside its own box — the outline could show one and the agent had no way to
 * advance it. Stepped rather than jumped, so lazy-loading handlers fire the way
 * they would for a person.
 */
export async function scrollRefBy(ref: string, deltaY: number): Promise<number> {
  const entry = refMap.get(ref)
  const objectId = await resolveObjectId(ref)
  const target = getActiveTarget()!
  emitAiAction({ kind: 'scroll', label: `AI scroll ${ref}`, detail: `${deltaY}px` })

  const can = (await target.dbg.sendCommand(
    'Runtime.callFunctionOn',
    {
      objectId,
      functionDeclaration:
        'function() { return this.scrollHeight > this.clientHeight }',
      returnByValue: true
    },
    entry?.sessionId
  )) as { result: { value: boolean } }
  if (!can.result.value) throw new Error('element does not scroll vertically')

  // Real wheel input over the element, not `scrollTop = n`.
  //
  // Assigning scrollTop moves the box but fires NO scroll event here: the
  // event is dispatched on a frame boundary, and an agent-driven window
  // produces none. Infinite scroll and lazy loading hang off that event, so
  // the container moved to its end and nothing ever appended — the position
  // was right and the page had not reacted at all.
  const point = await pointForRef(ref)
  const sign = deltaY >= 0 ? 1 : -1
  let left = Math.abs(deltaY)
  while (left > 0) {
    const step = Math.min(left, 120)
    await target.dbg.sendCommand('Input.dispatchMouseEvent', {
      type: 'mouseWheel',
      x: point.x,
      y: point.y,
      deltaX: 0,
      deltaY: sign * step
    })
    left -= step
    await new Promise((r) => setTimeout(r, 40))
  }

  const after = (await target.dbg.sendCommand(
    'Runtime.callFunctionOn',
    { objectId, functionDeclaration: 'function() { return this.scrollTop }', returnByValue: true },
    entry?.sessionId
  )) as { result: { value: number } }
  return after.result.value
}

/**
 * Press, move and release inside one element — a canvas stroke, a signature
 * pad, a colour picker. `dragRef` goes between two elements and cannot express
 * a gesture that starts and ends on the same surface.
 *
 * Offsets are fractions of the element's box, so they survive a resize.
 */
export async function drawOnRef(
  ref: string,
  from: { x: number; y: number },
  to: { x: number; y: number }
): Promise<void> {
  const entry = refMap.get(ref)
  const objectId = await resolveObjectId(ref)
  const target = getActiveTarget()!
  const res = (await target.dbg.sendCommand(
    'Runtime.callFunctionOn',
    {
      objectId,
      functionDeclaration: `async function() {
        ${SCROLL_INTO_VIEW}
        const r = this.getBoundingClientRect()
        return { left: r.left, top: r.top, w: r.width, h: r.height }
      }`,
      returnByValue: true,
      awaitPromise: true
    },
    entry?.sessionId
  )) as { result: { value: { left: number; top: number; w: number; h: number } } }

  const box = res.result.value
  const off = await measureFrameOffset(entry?.frameOwners)
  const pt = (f: { x: number; y: number }) => ({
    x: box.left + box.w * Math.min(1, Math.max(0, f.x)) + off.x,
    y: box.top + box.h * Math.min(1, Math.max(0, f.y)) + off.y
  })

  emitAiAction({ kind: 'click', label: `AI draw on ${ref}` })
  await thinkingPause()
  await humanDrag(pt(from), pt(to))
}

/**
 * Drag one ref onto another. Returns which mechanism actually ran, because
 * the two reach different libraries and a drag that changed nothing is
 * otherwise indistinguishable from one that worked.
 */
export async function dragRef(
  fromRef: string,
  toRef: string
): Promise<'native' | 'pointer' | 'synthetic'> {
  // Two passes. The first may scroll either element into view, which moves the
  // other one — measuring source then target once gives a source coordinate
  // that the target's scroll has already invalidated. By the second pass both
  // are on screen, so neither scrolls and the pair is consistent.
  await pointForRef(fromRef)
  await pointForRef(toRef)
  const from = await pointForRef(fromRef)
  const to = await pointForRef(toRef)
  emitAiAction({ kind: 'click', label: `AI drag ${fromRef} → ${toRef}` })
  await thinkingPause()
  // Try the browser's own drag machinery first: a draggable element takes over
  // on mousedown and mouse events after that are invisible to the page. Fall
  // back to the held-button gesture, which is what pointer-based sortables
  // (dnd-kit, SortableJS in pointer mode) listen for.
  const native = await nativeDrag(from, to)
  if (native) return 'native'

  const before = await dropSignature(toRef)
  await humanDrag(from, to)
  if ((await dropSignature(toRef)) !== before) return 'pointer'

  // Neither real-input path reached the page. That happens with HTML5
  // draggable elements, where the browser takes the gesture over on mousedown
  // and the mouse events that follow are invisible to the document. Synthesise
  // the drag events as a last resort — the DataTransfer is real and the
  // handlers run, but the events carry isTrusted=false, so the caller is told
  // which path ran rather than being left to assume the drag was genuine.
  await synthesiseDrag(fromRef, toRef)
  return 'synthetic'
}

/**
 * Core hover: same scroll → thinking pause → cursor move → flash sequence as
 * clickObjectId, but WITHOUT the press/release — the cursor just arrives and
 * stays, so :hover styles / mouseover-driven menus stay open. Shared by
 * hoverRef (snapshot ref) and hoverSelector (CSS selector).
 */
async function hoverObjectId(
  objectId: string,
  label: string,
  role?: string,
  frameOwners?: FrameOwner[],
  sessionId?: string
): Promise<void> {
  const target = getActiveTarget()!
  emitAiAction({ kind: 'hover', label, detail: role })

  const result = (await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: `async function() {
      ${SCROLL_INTO_VIEW}
      const r = this.getBoundingClientRect()
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 }
    }`,
    returnByValue: true,
    awaitPromise: true
  }, sessionId)) as { result: { value: { x: number; y: number } } }
  // getBoundingClientRect ran inside the element's own frame, so its origin is
  // that frame — mouse events are dispatched in page space. Measured after the
  // scrollIntoView above, which moves the page under the frame.
  const frameOffset = await measureFrameOffset(frameOwners)
  const x = result.result.value.x + frameOffset.x
  const y = result.result.value.y + frameOffset.y

  await thinkingPause()
  await humanMouseMove(x, y)

  await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: `function(label) {
      if (window.__reverAi) window.__reverAi.flashElement(this, label, 'hover')
    }`,
    arguments: [{ value: label }]
  }, sessionId)
}

/** Core type: same human-shaped sequence as clickObjectId, then focus + type
 * (+ optional Enter) via real CDP key events. Shared by typeRef/typeSelector. */
async function typeObjectId(
  objectId: string,
  text: string,
  submit: boolean,
  label: string,
  role?: string,
  frameOwners?: FrameOwner[],
  sessionId?: string
): Promise<void> {
  const target = getActiveTarget()!
  emitAiAction({ kind: 'type', label, detail: text.slice(0, 80) })

  const result = (await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: `async function() {
      ${SCROLL_INTO_VIEW}
      const r = this.getBoundingClientRect()
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 }
    }`,
    returnByValue: true,
    awaitPromise: true
  }, sessionId)) as { result: { value: { x: number; y: number } } }
  // getBoundingClientRect ran inside the element's own frame, so its origin is
  // that frame — mouse events are dispatched in page space. Measured after the
  // scrollIntoView above, which moves the page under the frame.
  const frameOffset = await measureFrameOffset(frameOwners)
  const x = result.result.value.x + frameOffset.x
  const y = result.result.value.y + frameOffset.y

  await thinkingPause()
  await humanMouseMove(x, y)

  await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: `function(label) {
      if (window.__reverAi) window.__reverAi.flashElement(this, label, 'type')
    }`,
    arguments: [{ value: label }]
  }, sessionId)
  await humanPressRelease(x, y)
  await humanType(objectId, text, submit, sessionId)
}

export async function clickRef(ref: string): Promise<void> {
  const entry = refMap.get(ref)
  const objectId = await resolveObjectId(ref)
  const label = `AI click${entry?.name ? ` "${entry.name.slice(0, 32)}"` : ''}`
  await clickObjectId(objectId, label, entry?.role, entry?.frameOwners, entry?.sessionId)
}

export async function typeRef(ref: string, text: string, submit: boolean): Promise<void> {
  const entry = refMap.get(ref)
  const objectId = await resolveObjectId(ref)
  const label = `AI type${entry?.name ? ` → "${entry.name.slice(0, 24)}"` : ''}`
  await typeObjectId(
    objectId,
    text,
    submit,
    label,
    entry?.role,
    entry?.frameOwners,
    entry?.sessionId
  )
}

/**
 * Choose option(s) in a `<select>`.
 *
 * A native dropdown has no DOM to click: the option list is drawn by the OS,
 * so the click and type paths cannot reach it. Setting `selected` and firing
 * input/change is what Playwright does too, and it is the only way a form
 * gated on a select can be completed — a real one blocked a card payment on
 * its mandatory installment field, with no tool able to answer it.
 *
 * Returns the labels actually chosen. On no match the available options come
 * back in the error, so the caller can retry without another snapshot.
 */
export async function selectRef(ref: string, values: string[]): Promise<string[]> {
  const entry = refMap.get(ref)
  const objectId = await resolveObjectId(ref)
  const target = getActiveTarget()!
  emitAiAction({ kind: 'type', label: `AI select${entry?.name ? ` "${entry.name.slice(0, 24)}"` : ''}`, detail: values.join(', ') })

  const res = (await target.dbg.sendCommand(
    'Runtime.callFunctionOn',
    {
      objectId,
      functionDeclaration: `function(values) {
        if (this.tagName !== 'SELECT') {
          return { error: 'ref is a <' + this.tagName.toLowerCase() + '>, not a <select>' }
        }
        const wanted = new Set(values)
        const picked = []
        for (const opt of this.options) {
          const label = (opt.label || opt.text || '').trim()
          const on = wanted.has(opt.value) || wanted.has(label)
          opt.selected = on
          if (on) picked.push(label || opt.value)
        }
        if (picked.length === 0) {
          return {
            error: 'no option matched',
            options: Array.from(this.options).map(o => (o.label || o.text || '').trim())
          }
        }
        this.dispatchEvent(new Event('input', { bubbles: true }))
        this.dispatchEvent(new Event('change', { bubbles: true }))
        return { picked }
      }`,
      arguments: [{ value: values }],
      returnByValue: true
    },
    entry?.sessionId
  )) as { result: { value: { picked?: string[]; error?: string; options?: string[] } } }

  const out = res.result.value
  if (out.error) {
    const list = out.options?.length ? ` — options: ${out.options.join(' | ')}` : ''
    throw new Error(`${out.error}${list}`)
  }
  return out.picked ?? []
}

/**
 * Attach files to a file input.
 *
 * A file input cannot be typed into — the browser owns its value, and clicking
 * it opens a native picker no tool can answer. `DOM.setFileInputFiles` is the
 * only way in. The ref usually points at the input's rendered button rather
 * than the input itself, so the element is walked up to the real
 * `input[type=file]` before the files are set.
 */
export async function uploadToRef(ref: string, paths: string[]): Promise<string[]> {
  const entry = refMap.get(ref)
  if (!entry) throw new Error(`unknown ref "${ref}" — call browser_snapshot first`)
  const missing = paths.filter((p) => !existsSync(p))
  if (missing.length) {
    throw new Error(`file(s) not found: ${missing.join(', ')} — pass absolute paths`)
  }

  const target = getActiveTarget()!
  const objectId = await resolveObjectId(ref)
  const input = (await target.dbg.sendCommand(
    'Runtime.callFunctionOn',
    {
      objectId,
      functionDeclaration: `function() {
        if (this.tagName === 'INPUT' && this.type === 'file') return this
        return this.closest?.('input[type=file]')
          ?? this.querySelector?.('input[type=file]')
          ?? null
      }`
    },
    entry.sessionId
  )) as { result: { objectId?: string; subtype?: string } }

  if (!input.result.objectId || input.result.subtype === 'null') {
    throw new Error(`ref "${ref}" is not a file input and does not contain one`)
  }

  await target.dbg.sendCommand(
    'DOM.setFileInputFiles',
    { files: paths, objectId: input.result.objectId },
    entry.sessionId
  )
  return paths
}

/**
 * Move back or forward in the tab's own history.
 *
 * Navigating to the previous URL is not the same thing: it pushes a new entry,
 * so the page sees a fresh load rather than a restore, and anything that keys
 * off history length or popstate behaves differently.
 */
export async function navigateHistory(direction: 'back' | 'forward'): Promise<string> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')

  const { currentIndex, entries } = (await target.dbg.sendCommand(
    'Page.getNavigationHistory'
  )) as { currentIndex: number; entries: Array<{ id: number; url: string }> }

  const next = direction === 'back' ? currentIndex - 1 : currentIndex + 1
  if (next < 0 || next >= entries.length) {
    throw new Error(`nothing to go ${direction} to — at entry ${currentIndex + 1} of ${entries.length}`)
  }
  await target.dbg.sendCommand('Page.navigateToHistoryEntry', { entryId: entries[next].id })
  emitAiAction({ kind: 'navigate', label: `AI ${direction}`, detail: entries[next].url })
  return entries[next].url
}

/**
 * Give a ref keyboard focus without clicking it.
 *
 * Clicking to focus is not always equivalent: a click on a dialog's backdrop
 * dismisses it, and a click on a listbox item selects the wrong row before the
 * arrow keys ever run.
 */
export async function focusRef(ref: string): Promise<void> {
  const entry = refMap.get(ref)
  const objectId = await resolveObjectId(ref)
  const target = getActiveTarget()!
  await target.dbg.sendCommand(
    'Runtime.callFunctionOn',
    {
      objectId,
      functionDeclaration: `function() {
        if (this.focus) this.focus()
        else if (this.tabIndex >= 0) this.focus()
      }`
    },
    entry?.sessionId
  )
}

export async function hoverRef(ref: string): Promise<void> {
  const entry = refMap.get(ref)
  const objectId = await resolveObjectId(ref)
  const label = `AI hover${entry?.name ? ` "${entry.name.slice(0, 32)}"` : ''}`
  await hoverObjectId(objectId, label, entry?.role, entry?.frameOwners, entry?.sessionId)
}

export async function hoverSelector(selector: string): Promise<void> {
  const objectId = await resolveSelectorObjectId(selector)
  const shortSel = selector.length > 32 ? selector.slice(0, 32) + '…' : selector
  await hoverObjectId(objectId, `AI hover "${shortSel}"`)
}

export async function clickSelector(selector: string): Promise<void> {
  const objectId = await resolveSelectorObjectId(selector)
  const shortSel = selector.length > 32 ? selector.slice(0, 32) + '…' : selector
  await clickObjectId(objectId, `AI click "${shortSel}"`)
}

export async function typeSelector(selector: string, text: string, submit: boolean): Promise<void> {
  const objectId = await resolveSelectorObjectId(selector)
  const shortSel = selector.length > 24 ? selector.slice(0, 24) + '…' : selector
  await typeObjectId(objectId, text, submit, `AI type → "${shortSel}"`)
}
