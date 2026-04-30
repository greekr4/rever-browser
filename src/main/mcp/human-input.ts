import { getActiveTarget } from '../chrome-cdp'

const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms))

const rand = (min: number, max: number): number => min + Math.random() * (max - min)

const easeInOutCubic = (t: number): number =>
  t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2

// Persisted pointer position so successive clicks travel from where the cursor
// "was" rather than warping from the corner each time.
let cursorX = 60 + Math.random() * 200
let cursorY = 60 + Math.random() * 200

export function getCursor(): { x: number; y: number } {
  return { x: cursorX, y: cursorY }
}

/**
 * CDP-driven mouse move from the persisted cursor position to (toX, toY) using
 * an eased path with jitter and per-step delay. Real mousemove events fire
 * along the path, so sites with behaviour-based bot detection see human-shaped
 * input rather than a teleport.
 */
export async function humanMouseMove(toX: number, toY: number): Promise<void> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')

  const fromX = cursorX
  const fromY = cursorY
  const dist = Math.hypot(toX - fromX, toY - fromY)
  // ~1 step per 25px, capped 12–60. Larger distance = more steps but each ~10ms.
  const steps = Math.max(12, Math.min(60, Math.round(dist / 25)))

  for (let i = 1; i <= steps; i++) {
    const t = i / steps
    const e = easeInOutCubic(t)
    const x = fromX + (toX - fromX) * e + rand(-1.5, 1.5)
    const y = fromY + (toY - fromY) * e + rand(-1.5, 1.5)
    // Dispatch the real mouse event AND update the visual cursor sprite in
    // parallel so the rendered cursor stays in sync with each CDP step.
    await Promise.all([
      target.dbg.sendCommand('Input.dispatchMouseEvent', {
        type: 'mouseMoved',
        x,
        y,
        button: 'none'
      }),
      target.dbg.sendCommand('Runtime.evaluate', {
        expression: `window.__reverAi && window.__reverAi.showCursorAt(${x}, ${y})`
      })
    ])
    await sleep(rand(6, 14))
  }
  cursorX = toX
  cursorY = toY
}

/** Press + release at the current cursor position (no movement). Caller
 * should `humanMouseMove` first. Includes a settle delay and realistic
 * press hold so the highlight has a moment to render before the click. */
export async function humanPressRelease(x: number, y: number): Promise<void> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')

  await sleep(rand(140, 260)) // settle — also lets the highlight overlay render
  await Promise.all([
    target.dbg.sendCommand('Input.dispatchMouseEvent', {
      type: 'mousePressed',
      x,
      y,
      button: 'left',
      clickCount: 1
    }),
    target.dbg.sendCommand('Runtime.evaluate', {
      expression: 'window.__reverAi && window.__reverAi.setCursorPress(true)'
    })
  ])
  await sleep(rand(45, 95))
  await Promise.all([
    target.dbg.sendCommand('Input.dispatchMouseEvent', {
      type: 'mouseReleased',
      x,
      y,
      button: 'left',
      clickCount: 1
    }),
    target.dbg.sendCommand('Runtime.evaluate', {
      expression: 'window.__reverAi && window.__reverAi.setCursorPress(false)'
    })
  ])
}

/** Pre-action "thinking" pause — looking at the page before acting. */
export async function thinkingPause(): Promise<void> {
  await sleep(rand(380, 950))
}

/**
 * Type the text into the currently-focused element progressively, char by
 * char, with realistic per-keystroke timing and the occasional thinking
 * pause. Uses Runtime.callFunctionOn so the same JS runs against the focused
 * element regardless of which page/frame it lives in.
 *
 * Sets value via the prototype setter (React-friendly) and dispatches input
 * events on every char, so search-as-you-type / autocomplete behave naturally.
 */
export async function humanType(
  objectId: string,
  text: string,
  submit: boolean
): Promise<void> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')

  await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: `async function(text, submit) {
      this.focus()
      const proto = this.tagName === 'TEXTAREA'
        ? window.HTMLTextAreaElement.prototype
        : window.HTMLInputElement.prototype
      const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set
      const setVal = (v) => { setter ? setter.call(this, v) : (this.value = v) }
      const sleep = (ms) => new Promise(r => setTimeout(r, ms))
      const rand = (a, b) => a + Math.random() * (b - a)

      let cur = this.value || ''
      for (const ch of Array.from(text)) {
        cur += ch
        setVal(cur)
        this.dispatchEvent(new Event('input', { bubbles: true }))
        // ~5–7 chars/sec normal, jitter wide
        await sleep(rand(55, 165))
        // 8% chance of a longer "thinking" pause mid-typing
        if (Math.random() < 0.08) await sleep(rand(220, 480))
      }
      this.dispatchEvent(new Event('change', { bubbles: true }))
      if (submit) {
        await sleep(rand(220, 480))
        this.dispatchEvent(new KeyboardEvent('keydown', {
          key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true
        }))
      }
    }`,
    arguments: [{ value: text }, { value: submit }],
    awaitPromise: true
  })
}

/**
 * Smooth scroll: break a single scroll request into many small wheel-sized
 * chunks with per-chunk delay. `deltaY` ~ total pixels (sign = direction).
 */
export async function humanScroll(
  totalDeltaY: number,
  absoluteY: number | undefined
): Promise<{ scrollY: number }> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')

  // Resolve to a concrete delta. If absoluteY supplied, compute current Y first.
  let delta = totalDeltaY
  if (typeof absoluteY === 'number') {
    const cur = (await target.dbg.sendCommand('Runtime.evaluate', {
      expression: 'window.scrollY',
      returnByValue: true
    })) as { result: { value: number } }
    delta = absoluteY - cur.result.value
  }

  const sign = delta >= 0 ? 1 : -1
  let remaining = Math.abs(delta)
  while (remaining > 0) {
    const chunk = Math.min(remaining, rand(80, 160))
    await target.dbg.sendCommand('Runtime.evaluate', {
      expression: `window.scrollBy(0, ${sign * chunk})`
    })
    remaining -= chunk
    await sleep(rand(28, 70))
  }

  const final = (await target.dbg.sendCommand('Runtime.evaluate', {
    expression: '({ scrollY: window.scrollY })',
    returnByValue: true
  })) as { result: { value: { scrollY: number } } }
  return final.result.value
}
