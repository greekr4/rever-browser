// Runs src/page_script.js against a small stand-in DOM and prints what it returned.
// Usage: node page_script_harness.cjs <path to page_script.js>
// (.cjs because the repository's package.json makes every .js an ES module.)
// The Rust tests in src/snapshot.rs run this and check the output, so the script is
// exercised as real JavaScript without a browser.
'use strict'

const fs = require('fs')

const INLINE = new Set(['SPAN', 'A', 'IMG', 'LABEL', 'B'])
const INLINE_BLOCK = new Set(['INPUT', 'BUTTON', 'SELECT'])
const STYLE_DEFAULTS = {
  opacity: '1', overflowX: 'visible', overflowY: 'visible', position: 'static', zIndex: 'auto',
  backgroundColor: 'rgba(0, 0, 0, 0)', borderLeftWidth: '0px', borderTopWidth: '0px',
  paddingLeft: '0px', paddingTop: '0px'
}
const INHERITED = { visibility: 'visible', cursor: 'auto' }

class TextNode {
  constructor(data) {
    this.nodeType = 3
    this.data = data
    this.parentNode = null
  }
}

class El {
  // spec: { attrs, style, rect: [left, top, width, height], props, shadow: [children] }
  constructor(tag, spec, kids) {
    spec = spec || {}
    this.nodeType = 1
    this.tagName = tag.toUpperCase()
    this.localName = tag.toLowerCase()
    this.attrs = spec.attrs || {}
    this.style = spec.style || {}
    this.rect = spec.rect || null
    this.parentNode = null
    this.childNodes = []
    this.shadowRoot = null
    this.isConnected = true
    this.scrollTop = 0
    this.events = []
    Object.assign(this, spec.props || {})
    for (const kid of kids || []) this.append(kid)
    if (spec.shadow) {
      this.shadowRoot = { nodeType: 11, host: this, childNodes: [] }
      for (const kid of spec.shadow) {
        const node = typeof kid === 'string' ? new TextNode(kid) : kid
        node.parentNode = this.shadowRoot
        this.shadowRoot.childNodes.push(node)
      }
    }
  }
  append(kid) {
    const node = typeof kid === 'string' ? new TextNode(kid) : kid
    node.parentNode = this
    this.childNodes.push(node)
  }
  get children() { return this.childNodes.filter((n) => n.nodeType === 1) }
  get parentElement() { return this.parentNode && this.parentNode.nodeType === 1 ? this.parentNode : null }
  get ownerDocument() {
    let node = this
    while (node && node.nodeType !== 9) node = node.parentNode || node.host
    return node
  }
  getAttribute(name) { return name in this.attrs ? this.attrs[name] : null }
  hasAttribute(name) { return name in this.attrs }
  getBoundingClientRect() {
    const [left, top, width, height] = this.rect || [0, 0, 0, 0]
    return { left, top, width, height, right: left + width, bottom: top + height }
  }
  // No rect means no box was generated at all, as for an option of a closed select.
  getClientRects() { return this.rect ? [this.getBoundingClientRect()] : [] }
  matches(selector) { return selector === ':disabled' && this.disabled === true }
  closest(selector) {
    const tags = selector.toUpperCase().split(',')
    for (let p = this.parentElement; p; p = p.parentElement) if (tags.includes(p.tagName)) return p
    return null
  }
  get textContent() {
    return this.childNodes.map((n) => (n.nodeType === 3 ? n.data : n.textContent)).join('')
  }
  focus() { this.ownerDocument.activeElement = this }
  scrollIntoView() { this.scrolled = true }
  dispatchEvent(event) { this.events.push(event.type) }
}

const el = (tag, spec, ...kids) => new El(tag, spec, kids)

function makeDocument(title, url, view, root) {
  const doc = {
    nodeType: 9, title, URL: url, defaultView: view, documentElement: root, activeElement: null,
    getElementById(id) {
      const find = (node) => {
        if (node.nodeType !== 1) return null
        if (node.attrs.id === id) return node
        for (const kid of node.childNodes) {
          const hit = find(kid)
          if (hit) return hit
        }
        return null
      }
      return find(root)
    },
    querySelector(selector) {
      if (selector[0] !== '#') throw new Error('unsupported selector')
      return doc.getElementById(selector.slice(1))
    }
  }
  root.parentNode = doc
  return doc
}

function computedStyle(node) {
  const style = Object.assign({}, STYLE_DEFAULTS, node.style)
  if (!style.display) {
    style.display = node.tagName === 'HEAD' ? 'none'
      : INLINE.has(node.tagName) ? 'inline'
      : INLINE_BLOCK.has(node.tagName) ? 'inline-block'
      : 'block'
  }
  for (const key of Object.keys(INHERITED)) {
    let value
    for (let p = node; p && value === undefined; p = p.parentNode || p.host) {
      value = p.style && p.style[key]
    }
    style[key] = value === undefined ? INHERITED[key] : value
  }
  return style
}

class Event {
  constructor(type) { this.type = type }
}

function makeView(extra) {
  return Object.assign({ getComputedStyle: computedStyle, Event, innerWidth: 1000, innerHeight: 800 }, extra)
}

// ---- the page ---------------------------------------------------------------

const win = makeView({ scrollX: 0, scrollY: 0, location: { href: 'https://shop.test/' } })

// A same-origin frame: its button is at (10,10) in the frame's own viewport.
const payView = makeView({ innerWidth: 396, innerHeight: 146 })
const payNow = el('button', { rect: [10, 10, 100, 40] }, 'Pay now')
const payDoc = makeDocument('Pay doc', 'https://shop.test/pay', payView,
  el('html', { rect: [0, 0, 396, 146] }, el('body', { rect: [0, 0, 396, 146] }, payNow)))
const payFrame = el('iframe', {
  attrs: { title: 'Pay' }, rect: [0, 600, 400, 150],
  style: { borderLeftWidth: '2px', borderTopWidth: '2px' }, props: { contentDocument: payDoc }
})
payView.frameElement = payFrame

const adsFrame = el('iframe', { attrs: { title: 'Ads' }, rect: [420, 600, 300, 150] })
Object.defineProperty(adsFrame, 'contentDocument', {
  get() { throw new Error('Blocked a frame with origin "https://shop.test" from accessing a cross-origin frame.') }
})

const beacon = el('iframe', { rect: [20, 760, 10, 10], props: { src: 'https://ads.test/beacon' } })
Object.defineProperty(beacon, 'contentDocument', { get() { throw new Error('cross-origin') } })

const pixelView = makeView({ innerWidth: 10, innerHeight: 10 })
const pixelDoc = makeDocument('Tracker', 'https://shop.test/pixel', pixelView,
  el('html', { rect: [0, 0, 10, 10] }, el('body', { rect: [0, 0, 10, 10] })))
const pixelFrame = el('iframe', { rect: [0, 760, 10, 10], props: { contentDocument: pixelDoc } })
pixelView.frameElement = pixelFrame

const buy = el('button', { attrs: { id: 'buy' }, rect: [20, 100, 100, 40] },
  el('span', { rect: [20, 100, 40, 40] }, 'Buy'), ' now')
const searchLabel = el('label', { attrs: { for: 'q' }, rect: [0, 150, 60, 20] }, 'Search')
const search = el('input', {
  attrs: { id: 'q', type: 'text' }, rect: [70, 150, 200, 30],
  props: { type: 'text', value: 'shoes', labels: [searchLabel] }
})
const password = el('input', {
  attrs: { type: 'password', 'aria-label': 'Password' }, rect: [0, 190, 200, 30],
  props: { type: 'password', value: 'abcd' }
})
const terms = el('input', {
  attrs: { type: 'checkbox' }, rect: [0, 230, 20, 20], props: { type: 'checkbox', checked: false }
})
// Holds nothing but its checkbox and the checkbox's name: Chrome leaves such a label out.
const termsLabel = el('label', { rect: [0, 230, 200, 20], props: { control: terms } }, terms, ' Accept terms')
terms.labels = [termsLabel]
// A label with content of its own stays.
const news = el('input', {
  attrs: { type: 'checkbox' }, rect: [600, 230, 20, 20], props: { type: 'checkbox', checked: true }
})
const newsLabel = el('label', { rect: [600, 230, 300, 20], props: { control: news } },
  news, ' Send me ', el('a', { attrs: { href: '/news' }, rect: [700, 230, 40, 20] }, 'news'))
news.labels = [newsLabel]

const small = el('option', { props: { value: 's', text: 'Small', label: 'Small', selected: false } }, 'Small')
const medium = el('option', { props: { value: 'm', text: 'Medium', label: 'Medium', selected: true } }, 'Medium')
const size = el('select', { attrs: { 'aria-label': 'Size' }, rect: [0, 260, 100, 30], props: { size: 0 } }, small, medium)
size.options = [small, medium]
Object.defineProperty(size, 'selectedOptions', { get() { return size.options.filter((o) => o.selected) } })

const orders = el('ul', {
  rect: [0, 460, 300, 100], style: { overflowY: 'auto' },
  props: { scrollWidth: 300, scrollHeight: 400, clientWidth: 300, clientHeight: 100 }
}, el('li', { rect: [0, 460, 300, 20], style: { display: 'list-item', listStyleType: 'disc' } }, 'Order 1'))
const numbered = { display: 'list-item', listStyleType: 'decimal' }
const steps = el('ol', { attrs: { start: '4' }, rect: [600, 300, 300, 60], props: { start: 4, reversed: false } },
  el('li', { rect: [600, 300, 300, 20], style: numbered, props: { value: 0 } }, 'Pay'),
  el('li', { attrs: { value: '9' }, rect: [600, 320, 300, 20], style: numbered, props: { value: 9 } }, 'Ship'),
  el('li', { rect: [600, 340, 300, 20], style: { display: 'list-item', listStyleType: 'none' }, props: { value: 0 } }, 'Done'))
const farBelow = el('button', { rect: [0, 5000, 100, 40] }, 'Far below')

const body = el('body', { rect: [0, 0, 1000, 5200] },
  el('header', { rect: [0, 0, 1000, 80] },
    el('h1', { rect: [0, 0, 300, 40] }, '\n  Shop &\n  Co '),
    el('nav', { attrs: { 'aria-label': 'Main' }, rect: [0, 40, 1000, 40] },
      el('a', { attrs: { href: '/' }, rect: [0, 40, 60, 40] }, 'Home'))),
  buy,
  searchLabel,
  search,
  password,
  termsLabel,
  size,
  el('button', { rect: [0, 300, 100, 40], props: { disabled: true } }, 'Sold out'),
  // The pointer cursor is set here and only inherited below: one click target, not two.
  el('div', { rect: [0, 350, 300, 100], style: { cursor: 'pointer' } },
    el('div', { rect: [0, 350, 300, 30] }, 'Card title')),
  orders,
  el('div', { style: { display: 'none' } }, el('button', { rect: [0, 0, 10, 10] }, 'Never rendered')),
  el('button', { rect: [0, 570, 100, 20], style: { opacity: '0' } }, 'Ghost'),
  el('span', { rect: [120, 570, 100, 20], style: { visibility: 'hidden' } }, 'secret'),
  el('img', { attrs: { alt: 'Logo' }, rect: [320, 100, 50, 50] }),
  el('div', { attrs: { 'aria-hidden': 'true' }, rect: [400, 100, 200, 100] },
    el('canvas', { attrs: { onclick: 'draw()' }, rect: [400, 100, 200, 100] })),
  el('my-widget', { rect: [320, 200, 200, 40], shadow: [el('button', { rect: [320, 200, 120, 40] }, 'Widget action')] }),
  newsLabel,
  steps,
  el('textarea', { attrs: { 'aria-label': 'Notes' }, rect: [600, 370, 300, 40], props: { value: 'line one\n\nline two' } }),
  // A handler set as a property, for an event other than click.
  el('div', { rect: [600, 420, 300, 20], props: { oncontextmenu() {} } }, 'Row menu'),
  // Nothing to say and nothing listening: Chrome leaves this canvas out.
  el('canvas', { rect: [600, 440, 100, 20] }),
  el('fieldset', { rect: [600, 470, 300, 40], props: { disabled: true } },
    el('legend', { rect: [600, 470, 60, 20] }, 'Billing'),
    el('input', { attrs: { type: 'text', 'aria-label': 'VAT' }, rect: [600, 490, 100, 20], props: { type: 'text', value: '', disabled: true } })),
  el('meter', { rect: [600, 520, 100, 20], props: { value: 0.6 } }),
  el('div', { attrs: { 'aria-label': 'Editor' }, rect: [600, 550, 300, 20], props: { isContentEditable: true } }, 'Draft'),
  payFrame,
  adsFrame,
  el('div', { attrs: { 'aria-hidden': 'true' }, rect: [0, 760, 10, 10] }, pixelFrame),
  // Cross-origin and hidden from the tree: no document to read and no line to hang on.
  el('div', { attrs: { 'aria-hidden': 'true' }, rect: [20, 760, 10, 10] }, beacon),
  farBelow)

const doc = makeDocument('Shop', 'https://shop.test/', win,
  el('html', { rect: [0, 0, 1000, 5200] }, el('head', {}, el('title', {}, 'Shop')), body))

globalThis.window = win
globalThis.document = doc
globalThis.location = win.location

// ---- run --------------------------------------------------------------------

const source = fs.readFileSync(process.argv[2], 'utf8')
const script = (0, eval)('(' + source + ')')

const capture = script('capture', { maxShadowDepth: 5, maxScannedElements: 10000, maxFrameDepth: 5, maxFrames: 24 })
const capture2 = script('capture', { maxShadowDepth: 5, maxScannedElements: 10000, maxFrameDepth: 5, maxFrames: 24 })

// The id the script gave an element, read back from its own state.
const state = win.__reverSnapshot
const id = (node) => state.ids.get(node)
const act = (target, action, value) => script('act', Object.assign({ action, value }, target))

const out = { capture, capture2 }
out.setRefs = script('setRefs', {
  r1: id(buy), r2: id(payNow), r3: id(search), r4: id(size), r5: id(orders), r6: id(farBelow)
})
out.lastDropped = state.last === null
out.globalHidden = !Object.keys(win).includes('__reverSnapshot')

out.point = act({ ref: 'r1' }, 'point')
out.scrolledVisible = buy.scrolled === true
out.framePoint = act({ ref: 'r2' }, 'point')
out.scrolledInFrame = payNow.scrolled === true
act({ ref: 'r6' }, 'point')
out.scrolledOffscreen = farBelow.scrolled === true
out.focus = act({ ref: 'r3' }, 'focus')
out.focused = doc.activeElement && doc.activeElement.tagName
out.setValue = act({ ref: 'r3' }, 'setValue', 'boots')
out.valueAfter = search.value
out.events = search.events
out.text = act({ ref: 'r3' }, 'text')
out.buttonText = act({ ref: 'r1' }, 'text')
out.setValueOnButton = act({ ref: 'r1' }, 'setValue', 'x')
out.select = act({ ref: 'r4' }, 'select', ['s'])
out.selectedAfter = size.options.map((o) => o.selected)
out.selectMissing = act({ ref: 'r4' }, 'select', ['xl'])
out.selectOnButton = act({ ref: 'r1' }, 'select', ['s'])
out.scrollState = act({ ref: 'r5' }, 'scrollState')
out.unknownRef = act({ ref: 'r99' }, 'point')
out.selectorPoint = act({ selector: '#buy' }, 'point')
out.selectorMissing = act({ selector: '#missing' }, 'point')
out.selectorInvalid = act({ selector: 'div >' }, 'point')
out.unknownAction = act({ ref: 'r1' }, 'dance')
buy.isConnected = false
out.detached = act({ ref: 'r1' }, 'point')

process.stdout.write(JSON.stringify(out))
