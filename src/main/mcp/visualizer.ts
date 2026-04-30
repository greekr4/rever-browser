// Injected at document-start into every webview document. Exposes
// `window.__reverAi` with helpers used by browser_click / browser_type / etc.
// to visualise AI-driven interactions on top of the page.
//
// Uses a closed Shadow DOM so page styles can't leak in/out. All elements live
// inside a single fixed-position host appended to documentElement.
export const VISUALIZER_INIT_SCRIPT = `
(() => {
  if (window.__reverAi) return
  const STYLE = \`
    .cursor {
      position: fixed;
      width: 22px; height: 22px;
      pointer-events: none;
      transform-origin: 2px 2px;
      transition: transform 90ms ease;
      filter: drop-shadow(0 1px 2px rgba(0,0,0,0.45));
      opacity: 0;
    }
    .cursor.visible { opacity: 1; }
    .cursor.press   { transform: scale(0.85); }
    .box {
      position: fixed;
      box-sizing: border-box;
      pointer-events: none;
      border-radius: 6px;
      animation: pop 220ms ease-out, fade 1300ms ease-in 700ms forwards;
    }
    .box.click   { border: 2px solid #ff3b30; box-shadow: 0 0 0 4px rgba(255,59,48,0.18); }
    .box.type    { border: 2px solid #0a84ff; box-shadow: 0 0 0 4px rgba(10,132,255,0.18); }
    .box.scroll  { border: 2px solid #30d158; box-shadow: 0 0 0 4px rgba(48,209,88,0.18); }
    .label {
      position: absolute;
      top: -22px; left: -2px;
      background: #111;
      color: #fff;
      font: 600 11px/1 -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;
      padding: 4px 6px;
      border-radius: 4px;
      white-space: nowrap;
      letter-spacing: 0.2px;
    }
    .box.click  .label { background: #ff3b30; }
    .box.type   .label { background: #0a84ff; }
    .box.scroll .label { background: #30d158; }
    .pulse {
      position: fixed;
      width: 14px; height: 14px;
      border-radius: 50%;
      background: rgba(255,59,48,0.85);
      box-shadow: 0 0 0 0 rgba(255,59,48,0.6);
      pointer-events: none;
      transform: translate(-50%, -50%);
      animation: ping 900ms ease-out forwards;
    }
    @keyframes pop  { 0% { transform: scale(0.9); opacity: 0 } 100% { transform: scale(1); opacity: 1 } }
    @keyframes fade { to { opacity: 0 } }
    @keyframes ping {
      0%   { box-shadow: 0 0 0 0 rgba(255,59,48,0.6); transform: translate(-50%,-50%) scale(0.6) }
      100% { box-shadow: 0 0 0 24px rgba(255,59,48,0); transform: translate(-50%,-50%) scale(1.4); opacity: 0 }
    }
  \`

  let host = null
  let root = null
  function ensure() {
    if (host && host.isConnected) return root
    host = document.createElement('div')
    host.id = '__rever_ai_overlay_host__'
    host.style.cssText = 'position:fixed;inset:0;width:0;height:0;z-index:2147483647;pointer-events:none'
    root = host.attachShadow({ mode: 'closed' })
    const s = document.createElement('style'); s.textContent = STYLE; root.appendChild(s)
    ;(document.documentElement || document.body).appendChild(host)
    return root
  }

  function flashRect(rect, label, action) {
    const r = ensure()
    const box = document.createElement('div')
    box.className = 'box ' + (action || 'click')
    box.style.left   = rect.x + 'px'
    box.style.top    = rect.y + 'px'
    box.style.width  = Math.max(rect.w, 8) + 'px'
    box.style.height = Math.max(rect.h, 8) + 'px'
    if (label) {
      const l = document.createElement('div')
      l.className = 'label'
      l.textContent = label
      box.appendChild(l)
    }
    r.appendChild(box)
    setTimeout(() => box.remove(), 2100)
  }

  function flashElement(el, label, action) {
    if (!el || !el.getBoundingClientRect) return
    const b = el.getBoundingClientRect()
    flashRect({ x: b.left, y: b.top, w: b.width, h: b.height }, label, action)
    pulseAt(b.left + b.width / 2, b.top + b.height / 2)
  }

  function pulseAt(x, y) {
    const r = ensure()
    const p = document.createElement('div')
    p.className = 'pulse'
    p.style.left = x + 'px'
    p.style.top  = y + 'px'
    r.appendChild(p)
    setTimeout(() => p.remove(), 950)
  }

// Fake cursor that follows real mousemove/down/up events. Because click/type
  // dispatch CDP Input.dispatchMouseEvent, those produce native mouse events
  // here — so this listener auto-tracks AI-driven movement without any IPC.
  // macOS-style arrow cursor SVG. The hot-spot is the tip (top-left) at (2,2),
  // which matches transform-origin in CSS so press-scale rotates around the tip.
  const CURSOR_SVG = '<svg viewBox="0 0 22 22" xmlns="http://www.w3.org/2000/svg">' +
    '<path d="M2 2 L2 18 L7 13.5 L10 19 L13 17.5 L10 12 L17 12 Z" ' +
    'fill="#fff" stroke="#000" stroke-width="1.2" stroke-linejoin="round"/></svg>'

  let cursorEl = null
  let cursorHideTimer = null
  function getCursor() {
    if (cursorEl && cursorEl.isConnected) return cursorEl
    const r = ensure()
    cursorEl = document.createElement('div')
    cursorEl.className = 'cursor'
    cursorEl.innerHTML = CURSOR_SVG
    r.appendChild(cursorEl)
    return cursorEl
  }
  function showCursorAt(x, y) {
    const c = getCursor()
    c.style.left = x + 'px'
    c.style.top = y + 'px'
    c.classList.add('visible')
    if (cursorHideTimer) clearTimeout(cursorHideTimer)
    cursorHideTimer = setTimeout(() => c.classList.remove('visible'), 1500)
  }
  function setCursorPress(pressed) {
    const c = getCursor()
    if (pressed) c.classList.add('press'); else c.classList.remove('press')
  }

  window.__reverAi = { flashRect, flashElement, pulseAt, showCursorAt, setCursorPress }
})();
`
