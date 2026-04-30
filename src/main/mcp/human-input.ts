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

// Map a single character to CDP Input.dispatchKeyEvent params. For ordinary
// printable chars this is enough; non-Latin (Korean, etc.) gets dispatched as
// a 'char' event with `text` set, which is how the IME path finally surfaces
// keys to the page.
function keyParamsFor(ch: string): {
  key: string
  code?: string
  keyCode?: number
  text: string
} {
  if (ch === ' ') return { key: ' ', code: 'Space', keyCode: 32, text: ' ' }
  if (ch === '\n') return { key: 'Enter', code: 'Enter', keyCode: 13, text: '\r' }
  const isAsciiLetter = /^[a-zA-Z]$/.test(ch)
  const isAsciiDigit = /^[0-9]$/.test(ch)
  if (isAsciiLetter) {
    const upper = ch.toUpperCase()
    return { key: ch, code: `Key${upper}`, keyCode: upper.charCodeAt(0), text: ch }
  }
  if (isAsciiDigit) {
    return { key: ch, code: `Digit${ch}`, keyCode: ch.charCodeAt(0), text: ch }
  }
  return { key: ch, text: ch }
}

/**
 * Type the text into the focused element via real CDP keyboard events. Each
 * char produces keyDown + keyUp dispatches with isTrusted=true, so behaviour-
 * based bot detectors (Naver Koop / Ncaptcha, Cloudflare Turnstile) see
 * authentic keystroke timing instead of a JS-dispatched event burst.
 *
 * The element is focused via Runtime.callFunctionOn first; per-char timing
 * uses the same jitter profile as humanMouseMove.
 */
export async function humanType(
  objectId: string,
  text: string,
  submit: boolean
): Promise<void> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')

  // Focus the target element. Required so the keyDown events land in it.
  await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: 'function(){ this.focus() }'
  })

  for (const ch of Array.from(text)) {
    const k = keyParamsFor(ch)
    await target.dbg.sendCommand('Input.dispatchKeyEvent', {
      type: 'keyDown',
      key: k.key,
      code: k.code,
      windowsVirtualKeyCode: k.keyCode,
      text: k.text,
      unmodifiedText: k.text
    })
    await target.dbg.sendCommand('Input.dispatchKeyEvent', {
      type: 'keyUp',
      key: k.key,
      code: k.code,
      windowsVirtualKeyCode: k.keyCode
    })
    await sleep(rand(55, 165))
    if (Math.random() < 0.08) await sleep(rand(220, 480))
  }

  if (submit) {
    await sleep(rand(220, 480))
    await target.dbg.sendCommand('Input.dispatchKeyEvent', {
      type: 'keyDown',
      key: 'Enter',
      code: 'Enter',
      windowsVirtualKeyCode: 13,
      text: '\r'
    })
    await target.dbg.sendCommand('Input.dispatchKeyEvent', {
      type: 'keyUp',
      key: 'Enter',
      code: 'Enter',
      windowsVirtualKeyCode: 13
    })
  }
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
