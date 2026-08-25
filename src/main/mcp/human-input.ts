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
    // The cursor sprite is cosmetic, so it is fired without waiting. Awaiting
    // it put a Runtime.evaluate on the critical path of every step, and that
    // call queues behind whatever the page's main thread is doing: typing three
    // characters into Naver's search box took 68 seconds, against 3 on an idle
    // page. Only the input event itself has to be awaited.
    void target.dbg
      .sendCommand('Runtime.evaluate', {
        expression: `window.__reverAi && window.__reverAi.showCursorAt(${x}, ${y})`
      })
      .catch(() => {})
    await target.dbg.sendCommand('Input.dispatchMouseEvent', {
      type: 'mouseMoved',
      x,
      y,
      button: 'none'
    })
    await sleep(rand(6, 14))
  }
  cursorX = toX
  cursorY = toY
}

/** Press + release at the current cursor position (no movement). Caller
 * should `humanMouseMove` first. Includes a settle delay and realistic
 * press hold so the highlight has a moment to render before the click. */
export async function humanPressRelease(
  x: number,
  y: number,
  opts: { button?: 'left' | 'right' | 'middle'; clickCount?: number } = {}
): Promise<void> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')
  const button = opts.button ?? 'left'
  // A double click is one press/release pair with clickCount 2, not two
  // separate clicks — two browser_click calls are seconds apart and the page
  // pairs nothing. Chromium wants the run of counts, so 1 then 2.
  const counts = opts.clickCount && opts.clickCount > 1 ? [1, opts.clickCount] : [1]
  if (button !== 'left' || counts.length > 1) {
    for (const clickCount of counts) {
      await target.dbg.sendCommand('Input.dispatchMouseEvent', {
        type: 'mousePressed',
        x,
        y,
        button,
        clickCount
      })
      await sleep(rand(30, 70))
      await target.dbg.sendCommand('Input.dispatchMouseEvent', {
        type: 'mouseReleased',
        x,
        y,
        button,
        clickCount
      })
      await sleep(rand(30, 60))
    }
    return
  }
  // Same reason as the cursor move: the press/release sprite is cosmetic, and
  // awaiting it queues behind the page's main thread.

  await sleep(rand(140, 260)) // settle — also lets the highlight overlay render
  void target.dbg
    .sendCommand('Runtime.evaluate', {
      expression: 'window.__reverAi && window.__reverAi.setCursorPress(true)'
    })
    .catch(() => {})
  await target.dbg.sendCommand('Input.dispatchMouseEvent', {
    type: 'mousePressed',
    x,
    y,
    button: 'left',
    clickCount: 1
  })
  await sleep(rand(45, 95))
  void target.dbg
    .sendCommand('Runtime.evaluate', {
      expression: 'window.__reverAi && window.__reverAi.setCursorPress(false)'
    })
    .catch(() => {})
  await target.dbg.sendCommand('Input.dispatchMouseEvent', {
    type: 'mouseReleased',
    x,
    y,
    button: 'left',
    clickCount: 1
  })
}

/**
 * Press at one point, move while held, release at another — one gesture.
 *
 * HTML5 drag-and-drop and pointer-based sortables both need the button to stay
 * down across the move; a click at the source followed by a click at the target
 * is two unrelated clicks and neither library sees a drag.
 */
export async function humanDrag(
  from: { x: number; y: number },
  to: { x: number; y: number }
): Promise<void> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')

  await humanMouseMove(from.x, from.y)
  await target.dbg.sendCommand('Input.dispatchMouseEvent', {
    type: 'mousePressed',
    x: from.x,
    y: from.y,
    button: 'left',
    clickCount: 1
  })
  await sleep(rand(80, 160))

  // Intermediate moves with the button held. A single jump to the target is
  // ignored by libraries that require a drag threshold to be crossed.
  const steps = 14
  for (let i = 1; i <= steps; i++) {
    const t = easeInOutCubic(i / steps)
    const x = from.x + (to.x - from.x) * t
    const y = from.y + (to.y - from.y) * t
    await target.dbg.sendCommand('Input.dispatchMouseEvent', {
      type: 'mouseMoved',
      x,
      y,
      button: 'left',
      buttons: 1
    })
    await sleep(rand(10, 22))
  }

  await sleep(rand(60, 140))
  await target.dbg.sendCommand('Input.dispatchMouseEvent', {
    type: 'mouseReleased',
    x: to.x,
    y: to.y,
    button: 'left',
    clickCount: 1
  })
  cursorX = to.x
  cursorY = to.y
}

/**
 * HTML5 drag-and-drop, which mouse events alone cannot drive.
 *
 * A `draggable` element hands off to the browser's own drag machinery on
 * mousedown, and from there Input.dispatchMouseEvent produces nothing the page
 * can see — the fixture's drop target stayed silent through a full press,
 * move and release. Interception turns the gesture into dragIntercepted, whose
 * payload is the DragData the drop needs.
 */
export async function nativeDrag(
  from: { x: number; y: number },
  to: { x: number; y: number }
): Promise<boolean> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')

  const dragData = await new Promise<unknown>((resolve) => {
    const timer = setTimeout(() => {
      target.dbg.off('message', onMessage)
      resolve(null)
    }, 1500)
    function onMessage(_e: unknown, method: string, params: unknown): void {
      if (method !== 'Input.dragIntercepted') return
      clearTimeout(timer)
      target!.dbg.off('message', onMessage)
      resolve((params as { data: unknown }).data)
    }
    target.dbg.on('message', onMessage)
    void (async () => {
      await target.dbg
        .sendCommand('Input.setInterceptDrags', { enabled: true })
        .catch((e) => console.warn('[drag] setInterceptDrags:', e?.message ?? e))
      await humanMouseMove(from.x, from.y)
      await target.dbg.sendCommand('Input.dispatchMouseEvent', {
        type: 'mousePressed',
        x: from.x,
        y: from.y,
        button: 'left',
        clickCount: 1
      })
      await sleep(60)
      await target.dbg.sendCommand('Input.dispatchMouseEvent', {
        type: 'mouseMoved',
        x: from.x + 12,
        y: from.y + 12,
        button: 'left',
        buttons: 1
      })
    })()
  })

  if (!dragData) {
    // The press above is still held. Leaving it down breaks the fallback,
    // which starts with a press of its own and would arrive as a second one.
    await target.dbg
      .sendCommand('Input.dispatchMouseEvent', {
        type: 'mouseReleased',
        x: from.x + 12,
        y: from.y + 12,
        button: 'left',
        clickCount: 1
      })
      .catch(() => {})
    await target.dbg.sendCommand('Input.setInterceptDrags', { enabled: false }).catch(() => {})
    return false
  }

  // dragEnter establishes the current drop target. Without it Chromium has
  // nothing to hand the following dragOver/drop to, and the sequence dispatches
  // cleanly while the page sees nothing.
  const steps = 10
  for (let i = 1; i <= steps; i++) {
    const t = easeInOutCubic(i / steps)
    const x = from.x + (to.x - from.x) * t
    const y = from.y + (to.y - from.y) * t
    if (i === 1) {
      await target.dbg.sendCommand('Input.dispatchDragEvent', {
        type: 'dragEnter',
        x,
        y,
        data: dragData
      })
    }
    await target.dbg.sendCommand('Input.dispatchDragEvent', {
      type: 'dragOver',
      x,
      y,
      data: dragData
    })
    await sleep(rand(14, 28))
  }
  await target.dbg.sendCommand('Input.dispatchDragEvent', {
    type: 'dragEnter',
    x: to.x,
    y: to.y,
    data: dragData
  })
  await target.dbg.sendCommand('Input.dispatchDragEvent', {
    type: 'dragOver',
    x: to.x,
    y: to.y,
    data: dragData
  })
  await target.dbg.sendCommand('Input.dispatchDragEvent', {
    type: 'drop',
    x: to.x,
    y: to.y,
    data: dragData
  })
  await target.dbg.sendCommand('Input.setInterceptDrags', { enabled: false }).catch(() => {})
  cursorX = to.x
  cursorY = to.y
  return true
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
 * Named keys that carry no printable text, plus the few that do.
 *
 * Typing cannot stand in for these: a dialog closes on Escape, a listbox moves
 * on ArrowDown, and a field is cleared with Control+A then Delete. Without them
 * a keyboard-driven widget cannot be operated at all.
 */
const NAMED_KEYS: Record<string, { code: string; keyCode: number; text?: string }> = {
  Enter: { code: 'Enter', keyCode: 13, text: '\r' },
  Escape: { code: 'Escape', keyCode: 27 },
  Tab: { code: 'Tab', keyCode: 9, text: '\t' },
  Backspace: { code: 'Backspace', keyCode: 8 },
  Delete: { code: 'Delete', keyCode: 46 },
  ArrowUp: { code: 'ArrowUp', keyCode: 38 },
  ArrowDown: { code: 'ArrowDown', keyCode: 40 },
  ArrowLeft: { code: 'ArrowLeft', keyCode: 37 },
  ArrowRight: { code: 'ArrowRight', keyCode: 39 },
  Home: { code: 'Home', keyCode: 36 },
  End: { code: 'End', keyCode: 35 },
  PageUp: { code: 'PageUp', keyCode: 33 },
  PageDown: { code: 'PageDown', keyCode: 34 },
  Space: { code: 'Space', keyCode: 32, text: ' ' }
}

const MODIFIER_BITS: Record<string, number> = { Alt: 1, Control: 2, Meta: 4, Shift: 8 }

/**
 * Editing shortcuts Chromium routes through named commands rather than through
 * the key event itself. Dispatching Meta+A without this moves nothing: the
 * following Delete then eats a single character instead of the selection,
 * which looked like "clearing a field almost works".
 */
const EDIT_COMMANDS: Record<string, string> = {
  a: 'selectAll',
  c: 'copy',
  v: 'paste',
  x: 'cut',
  z: 'undo'
}

/** The accelerator modifier this platform uses for editing shortcuts. */
const ACCEL = process.platform === 'darwin' ? 'Meta' : 'Control'

export function isNamedKey(key: string): boolean {
  return key in NAMED_KEYS
}

/** Every key this accepts, for an error message that can be acted on. */
export function namedKeys(): string[] {
  return Object.keys(NAMED_KEYS)
}

/**
 * Send one key to whatever currently has focus, as real CDP key events.
 *
 * `sessionId` routes the events' companion focus call; the key events
 * themselves always go to the page session, because they follow focus rather
 * than coordinates.
 */
export async function pressKey(
  key: string,
  opts: { modifiers?: string[]; repeat?: number } = {}
): Promise<void> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')

  // A webview that has not been interacted with DROPS key events and reports
  // no error — pressing Escape straight after a navigation silently did
  // nothing. Only a real mouse press or focusing an element grants the
  // renderer keyboard focus; Page.bringToFront, window.focus() and
  // body.focus() were all measured not to. document.hasFocus() separates the
  // two states exactly, so refuse rather than pretend the key was delivered.
  // An agent drives this app while it sits behind another window, so nothing
  // inside it holds OS focus and the page is treated as unfocused. Focus
  // emulation makes the renderer behave as if it were frontmost, which is what
  // a real mouse press achieves as a side effect.
  await target.dbg
    .sendCommand('Emulation.setFocusEmulationEnabled', { enabled: true })
    .catch(() => {})

  const focused = (await target.dbg.sendCommand('Runtime.evaluate', {
    expression: 'document.hasFocus()',
    returnByValue: true
  })) as { result: { value: boolean } }
  if (!focused.result.value) {
    throw new Error(
      'the page does not have keyboard focus, so the key would be dropped — pass ref to focus an element first, or click something'
    )
  }

  const named = NAMED_KEYS[key]
  const single = !named && Array.from(key).length === 1
  if (!named && !single) {
    throw new Error(`unknown key "${key}" — use a single character or one of: ${namedKeys().join(', ')}`)
  }
  const k = named ?? keyParamsFor(key)
  const modifiers = (opts.modifiers ?? []).reduce((m, name) => {
    const bit = MODIFIER_BITS[name]
    if (!bit) throw new Error(`unknown modifier "${name}" — use Alt, Control, Meta or Shift`)
    return m | bit
  }, 0)

  // A modified key is a shortcut, not text. Sending `text` alongside makes
  // Chromium treat Control+A as typing the letter "a" into the field.
  const text = modifiers === 0 ? ('text' in k ? k.text : undefined) : undefined

  const command =
    (opts.modifiers ?? []).includes(ACCEL) && !(opts.modifiers ?? []).includes('Shift')
      ? EDIT_COMMANDS[key.toLowerCase()]
      : undefined

  for (let i = 0; i < Math.max(1, opts.repeat ?? 1); i++) {
    // An editing command rides on a full keyDown; rawKeyDown carries the key
    // but Chromium never runs the command, which reads as "the shortcut did
    // nothing" rather than as an error.
    await target.dbg.sendCommand('Input.dispatchKeyEvent', {
      type: text || command ? 'keyDown' : 'rawKeyDown',
      key,
      code: k.code,
      windowsVirtualKeyCode: k.keyCode,
      modifiers,
      ...(command ? { commands: [command] } : {}),
      ...(text ? { text, unmodifiedText: text } : {})
    })
    await sleep(rand(20, 60))
    await target.dbg.sendCommand('Input.dispatchKeyEvent', {
      type: 'keyUp',
      key,
      code: k.code,
      windowsVirtualKeyCode: k.keyCode,
      modifiers
    })
    if (i > 0) await sleep(rand(30, 80))
  }
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
  submit: boolean,
  sessionId?: string
): Promise<void> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')

  // Focus the target element. Required so the keyDown events land in it.
  // The objectId belongs to whichever session resolved it — an out-of-process
  // frame's node is unknown to the page session. The key events that follow
  // stay on the page session: they go to whatever is focused, and focusing the
  // frame's input is what routes them into it.
  await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: 'function(){ this.focus() }'
  }, sessionId)

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
    // Typing beam + sparks — cosmetic, fire-and-forget for the same reason as
    // showCursorAt: awaiting would queue behind the page's main thread. Sent to
    // the focused element's session so an OOPIF input gets its own frame's beam.
    void target.dbg
      .sendCommand('Runtime.evaluate', {
        expression: 'window.__reverAi && window.__reverAi.typeKey()'
      }, sessionId)
      .catch(() => {})
    await sleep(rand(15, 55))
    if (Math.random() < 0.04) await sleep(rand(120, 260))
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

  // Real wheel input rather than window.scrollBy.
  //
  // scrollBy moves the page but fires no scroll event here — the event is
  // dispatched on a frame boundary and an agent-driven window produces none.
  // Infinite scroll, sticky headers and lazy images all hang off that event,
  // so the page moved and never reacted. A wheel goes through the browser's
  // input path and behaves the way a person's does, including handing the
  // scroll to whatever container is under the cursor.
  const { x, y } = getCursor()
  const sign = delta >= 0 ? 1 : -1
  let remaining = Math.abs(delta)
  while (remaining > 0) {
    const chunk = Math.min(remaining, 100 + rand(-15, 15))
    await target.dbg.sendCommand('Input.dispatchMouseEvent', {
      type: 'mouseWheel',
      x,
      y,
      deltaX: 0,
      deltaY: sign * chunk
    })
    remaining -= chunk
    await sleep(rand(14, 26))
  }

  const final = (await target.dbg.sendCommand('Runtime.evaluate', {
    expression: '({ scrollY: window.scrollY })',
    returnByValue: true
  })) as { result: { value: { scrollY: number } } }
  return final.result.value
}
