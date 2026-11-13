// Page-side half of the snapshot; the Rust half is snapshot.rs.
//
// Evaluated in the top document as `(<this file>)(command, argument)`. It replaces what
// the Electron app read over the Chrome DevTools Protocol: the accessibility tree
// (Accessibility.getFullAXTree), the layout capture (DOMSnapshot.captureSnapshot), the
// click scan and the per-frame trees, and it reports them in those payload shapes so the
// Rust side builds the outline with the same code paths.
//
// Everything is synchronous and the result is a JSON string: an engine's evaluate does
// not have to await promises or convert deep objects for this to work.
function (command, arg) {
  'use strict'

  const KEY = '__reverSnapshot'
  let state = window[KEY]
  if (!state) {
    // ids: one stable number per node for as long as the node lives, which is what a
    // backendNodeId was. Held weakly, so a snapshot does not keep removed elements alive.
    // refs: the elements the current snapshot gave a ref to.
    state = { ids: new WeakMap(), next: 1, last: null, refs: new Map() }
    // Non-enumerable, so a page that lists its own globals does not see it.
    Object.defineProperty(window, KEY, { value: state, configurable: true })
  }

  const idOf = (node) => {
    let id = state.ids.get(node)
    if (id === undefined) {
      id = state.next++
      state.ids.set(node, id)
    }
    return id
  }
  const viewOf = (el) => (el.ownerDocument && el.ownerDocument.defaultView) || window
  const styleOf = (el) => viewOf(el).getComputedStyle(el)
  const num = (v) => parseFloat(v) || 0
  const attr = (el, name) => (el.getAttribute(name) || '').trim()
  // HTML whitespace only: a no-break space is content, not layout.
  const squash = (s) => s.replace(/[ \t\n\r\f]+/g, ' ')

  // ---- accessibility facts ------------------------------------------------

  const SKIP_TAGS = new Set(['SCRIPT', 'STYLE', 'TEMPLATE', 'NOSCRIPT', 'HEAD'])
  const ARIA_ROLES = new Set(
    ('alert alertdialog application article banner blockquote button caption cell checkbox code ' +
      'columnheader combobox complementary contentinfo definition deletion dialog directory document ' +
      'emphasis feed figure form generic grid gridcell group heading img image insertion link list ' +
      'listbox listitem log main marquee math menu menubar menuitem menuitemcheckbox menuitemradio ' +
      'meter navigation none note option paragraph presentation progressbar radio radiogroup region ' +
      'row rowgroup rowheader scrollbar search searchbox separator slider spinbutton status strong ' +
      'subscript superscript switch tab table tablist tabpanel term textbox time timer toolbar ' +
      'tooltip tree treegrid treeitem').split(' ')
  )
  // Role names are the ones Chrome reports over CDP, because the outline prints them.
  const TAG_ROLES = new Map(Object.entries({
    BUTTON: 'button', TEXTAREA: 'textbox', OPTION: 'option', OPTGROUP: 'group',
    H1: 'heading', H2: 'heading', H3: 'heading', H4: 'heading', H5: 'heading', H6: 'heading',
    P: 'paragraph', UL: 'list', OL: 'list', MENU: 'list', LI: 'listitem',
    DL: 'DescriptionList', DT: 'term', DD: 'definition',
    NAV: 'navigation', MAIN: 'main', ASIDE: 'complementary', ARTICLE: 'article', FORM: 'form',
    FIELDSET: 'group', LEGEND: 'Legend', LABEL: 'LabelText',
    TABLE: 'table', THEAD: 'rowgroup', TFOOT: 'rowgroup', TR: 'row', TD: 'cell',
    CAPTION: 'caption', HR: 'separator', DIALOG: 'dialog', FIGURE: 'figure', FIGCAPTION: 'Figcaption',
    BLOCKQUOTE: 'blockquote', CODE: 'code', STRONG: 'strong', EM: 'emphasis', MARK: 'mark',
    TIME: 'time', ABBR: 'Abbr', SUB: 'subscript', SUP: 'superscript', DEL: 'deletion',
    INS: 'insertion', ADDRESS: 'group', OUTPUT: 'status', PROGRESS: 'progressbar', METER: 'meter',
    IFRAME: 'Iframe', FRAME: 'Iframe', CANVAS: 'Canvas', VIDEO: 'Video', AUDIO: 'Audio',
    EMBED: 'EmbeddedObject', OBJECT: 'PluginObject', BR: 'LineBreak',
    SUMMARY: 'DisclosureTriangle', DETAILS: 'group', HGROUP: 'group', SEARCH: 'search',
    SVG: 'image', MATH: 'MathMLMath'
  }))
  const INPUT_ROLES = new Map(Object.entries({
    button: 'button', submit: 'button', reset: 'button', image: 'button', file: 'button',
    checkbox: 'checkbox', radio: 'radio', range: 'slider', number: 'spinbutton',
    search: 'searchbox', color: 'ColorWell', date: 'Date', 'datetime-local': 'DateTime',
    month: 'DateTime', week: 'DateTime', time: 'InputTime'
  }))
  const NAME_FROM_CONTENT = new Set([
    'button', 'link', 'heading', 'option', 'menuitem', 'menuitemcheckbox', 'menuitemradio', 'tab',
    'checkbox', 'radio', 'switch', 'treeitem', 'cell', 'gridcell', 'columnheader',
    'rowheader', 'tooltip', 'term', 'DisclosureTriangle'
  ])
  const CHECKABLE = new Set(['checkbox', 'radio', 'switch', 'menuitemcheckbox', 'menuitemradio'])
  const VALUE_FROM_ARIA = new Set(['slider', 'spinbutton', 'progressbar', 'scrollbar', 'meter'])
  const NO_VALUE_INPUTS = new Set(['checkbox', 'radio', 'button', 'submit', 'reset', 'image', 'hidden'])
  // Inputs whose value is text typed into the field itself.
  const TEXT_INPUTS = new Set(['text', 'search', 'tel', 'url', 'email', 'password', 'number'])

  const inputType = (el) => String(el.type || 'text').toLowerCase()

  const roleOf = (el, tag) => {
    // The first role the author listed that exists; an unknown one falls back to the tag.
    const explicit = attr(el, 'role').toLowerCase().split(/\s+/).find((r) => ARIA_ROLES.has(r))
    if (explicit) {
      return explicit === 'presentation' ? 'none' : explicit === 'img' ? 'image' : explicit
    }
    const role = tagRole(el, tag)
    // Chrome reports an element the author made draggable as a group.
    return role === 'generic' && el.getAttribute('draggable') === 'true' ? 'group' : role
  }

  const tagRole = (el, tag) => {
    switch (tag) {
      case 'A':
      case 'AREA':
        return el.hasAttribute('href') ? 'link' : 'generic'
      case 'INPUT':
        return INPUT_ROLES.get(inputType(el)) || (el.hasAttribute('list') ? 'combobox' : 'textbox')
      case 'SELECT':
        return el.multiple || el.size > 1 ? 'listbox' : 'combobox'
      case 'IMG':
        // alt="" is the author saying the image is decoration.
        return el.getAttribute('alt') === '' ? 'none' : 'image'
      case 'TH':
        return attr(el, 'scope') === 'row' ? 'rowheader' : 'columnheader'
      case 'HEADER':
      case 'FOOTER': {
        // Only a page-level header or footer is a landmark.
        const scoped = el.closest('article,aside,main,nav,section')
        if (tag === 'HEADER') return scoped ? 'sectionheader' : 'banner'
        return scoped ? 'sectionfooter' : 'contentinfo'
      }
      case 'SECTION':
        // A section is a landmark only once it is named.
        return el.hasAttribute('aria-label') || el.hasAttribute('aria-labelledby') ? 'region' : 'generic'
    }
    return TAG_ROLES.get(tag) || 'generic'
  }

  // The rendered children: a shadow tree replaces the light children, and a slot shows
  // what was assigned to it.
  const flatKids = (el) => {
    if (el.shadowRoot) return el.shadowRoot.childNodes
    if (el.localName === 'slot' && el.assignedNodes) {
      const assigned = el.assignedNodes({ flatten: true })
      if (assigned.length) return assigned
    }
    return el.childNodes
  }

  // Text an element contributes to a name computed from content.
  const textOf = (el) => {
    let out = ''
    const kids = flatKids(el)
    for (let i = 0; i < kids.length; i++) {
      const c = kids[i]
      if (c.nodeType === 3) {
        out += c.data
        continue
      }
      if (c.nodeType !== 1) continue
      const tag = c.tagName.toUpperCase()
      // A control inside a label contributes its value, not its option list.
      if (SKIP_TAGS.has(tag) || tag === 'SELECT' || tag === 'TEXTAREA') continue
      if (c.getAttribute('aria-hidden') === 'true') continue
      const cs = styleOf(c)
      if (!cs || cs.display === 'none' || cs.visibility !== 'visible') continue
      const inner = attr(c, 'aria-label') || (tag === 'IMG' ? attr(c, 'alt') : '') || textOf(c)
      out += String(cs.display).indexOf('inline') === 0 ? inner : ' ' + inner + ' '
    }
    return out
  }
  const contentName = (el) => squash(textOf(el)).trim()

  const firstChild = (el, tag) => {
    for (let i = 0; i < el.children.length; i++) {
      if (el.children[i].tagName.toUpperCase() === tag) return el.children[i]
    }
    return null
  }

  const nameOf = (el, tag, role) => {
    const by = attr(el, 'aria-labelledby')
    if (by) {
      const parts = by.split(/\s+/).map((id) => {
        const target = el.ownerDocument.getElementById(id)
        return target ? textOf(target) : ''
      })
      const name = squash(parts.join(' ')).trim()
      if (name) return name
    }
    const label = squash(attr(el, 'aria-label'))
    if (label) return label

    if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA') {
      if (el.labels && el.labels.length) {
        const name = squash(Array.prototype.map.call(el.labels, textOf).join(' ')).trim()
        if (name) return name
      }
      if (tag === 'INPUT') {
        const type = inputType(el)
        if (type === 'submit' || type === 'reset' || type === 'button') {
          const name = el.value || (type === 'submit' ? 'Submit' : type === 'reset' ? 'Reset' : '')
          if (name) return name
        }
        if (type === 'image' && attr(el, 'alt')) return attr(el, 'alt')
        // The label of the browser's own button, as Chrome words it.
        if (type === 'file') return 'Choose File'
      }
      if (attr(el, 'placeholder')) return attr(el, 'placeholder')
    }
    if (tag === 'OPTGROUP' && attr(el, 'label')) return attr(el, 'label')
    if ((tag === 'IMG' || tag === 'AREA') && attr(el, 'alt')) return attr(el, 'alt')
    const caption =
      tag === 'FIELDSET' ? firstChild(el, 'LEGEND')
      : tag === 'TABLE' ? firstChild(el, 'CAPTION')
      : tag === 'SVG' ? firstChild(el, 'TITLE')
      : null
    if (caption) {
      const name = contentName(caption)
      if (name) return name
    }
    if (NAME_FROM_CONTENT.has(role)) {
      const name = contentName(el)
      if (name) return name
    }
    // On a plain container a title is a tooltip, not a name.
    return role === 'generic' || role === 'none' ? '' : attr(el, 'title')
  }

  const valueOf = (el, tag, role) => {
    if (tag === 'INPUT') {
      const type = inputType(el)
      if (NO_VALUE_INPUTS.has(type)) return ''
      if (type === 'file') return el.files && el.files.length ? el.files[0].name : 'No file chosen'
      const value = String(el.value == null ? '' : el.value)
      // What the user sees, so a typed password never reaches the agent's context.
      return type === 'password' ? '•'.repeat(value.length) : value
    }
    if (tag === 'TEXTAREA') return String(el.value == null ? '' : el.value)
    if (tag === 'SELECT') {
      if (role !== 'combobox') return ''
      const chosen = el.selectedOptions && el.selectedOptions[0]
      return chosen ? squash(chosen.label || chosen.text || '').trim() : ''
    }
    // Chrome keeps these as single-precision floats, and the outline prints what it kept.
    if (tag === 'PROGRESS' || tag === 'METER') return String(Math.fround(el.value))
    if (VALUE_FROM_ARIA.has(role)) return attr(el, 'aria-valuenow')
    // An editable region's value is what has been typed into it. Only the element that
    // starts the region carries it, not what sits inside.
    const host = el.isContentEditable && !(el.parentElement && el.parentElement.isContentEditable)
    if (host && (role === 'textbox' || role === 'searchbox' || role === 'generic')) return contentName(el)
    return ''
  }

  // Name, value and the states the outline prints, in the order Chrome lists them.
  const describe = (el, tag, role, node) => {
    const name = nameOf(el, tag, role)
    if (name) node.name = { value: name }
    const value = valueOf(el, tag, role)
    if (value !== '') node.value = { value: value }

    const props = []
    // A disabled fieldset or optgroup disables what is inside; Chrome flags only those.
    const group = tag === 'FIELDSET' || tag === 'OPTGROUP'
    if ((!group && el.matches && el.matches(':disabled')) || attr(el, 'aria-disabled') === 'true') {
      props.push({ name: 'disabled', value: { value: true } })
    }
    if (role === 'heading') {
      const level = parseInt(attr(el, 'aria-level'), 10) || (/^H[1-6]$/.test(tag) ? Number(tag[1]) : 2)
      props.push({ name: 'level', value: { value: level } })
    }
    if (tag === 'LI') {
      // How many lists deep the item sits.
      let level = 0
      for (let p = el.parentElement; p; p = p.parentElement) {
        if (p.tagName === 'UL' || p.tagName === 'OL' || p.tagName === 'MENU') level++
      }
      if (level) props.push({ name: 'level', value: { value: level } })
    }
    if (CHECKABLE.has(role)) {
      // A tristate, reported as a string: "false" is a value here, not an absence.
      let checked
      if (tag === 'INPUT') checked = el.indeterminate ? 'mixed' : el.checked ? 'true' : 'false'
      else {
        const aria = attr(el, 'aria-checked')
        checked = aria === 'true' || aria === 'mixed' ? aria : 'false'
      }
      props.push({ name: 'checked', value: { value: checked } })
    }
    const parent = el.parentElement
    if (attr(el, 'aria-expanded') === 'true' || (tag === 'SUMMARY' && parent && parent.open === true)) {
      props.push({ name: 'expanded', value: { value: true } })
    }
    if (tag === 'OPTION' ? el.selected === true : attr(el, 'aria-selected') === 'true') {
      props.push({ name: 'selected', value: { value: true } })
    }
    if (props.length) node.properties = props
  }

  // Chrome leaves out a label that only repeats the name of its checkbox or radio:
  // the control is named from the label, and the label holds nothing else.
  const redundantLabel = (el) => {
    const input = el.control
    if (!input || input.tagName !== 'INPUT' || (input.type !== 'checkbox' && input.type !== 'radio')) return false
    if (attr(input, 'aria-label') || attr(input, 'aria-labelledby')) return false
    const kids = el.children
    return kids.length === 0 || (kids.length === 1 && kids[0] === input)
  }

  // The bullet or number Chrome reports as a list item's first child.
  // shortcut: bullets and decimal numbers only; other numbering styles (letters, roman)
  // get no marker line. Add them here if an outline needs to match on those lists.
  const BULLETS = { disc: '\u2022', circle: '\u25e6', square: '\u25a0' }
  const listMarker = (el, cs, ordinals) => {
    if (cs.display !== 'list-item' || (cs.listStyleImage && cs.listStyleImage !== 'none')) return ''
    const type = cs.listStyleType
    if (BULLETS[type]) return BULLETS[type]
    const list = el.parentElement
    if (type !== 'decimal' || !list) return ''
    let known = ordinals.get(list)
    if (!known) {
      known = new Map()
      const items = Array.prototype.filter.call(list.children, (c) => c.tagName === 'LI')
      const step = list.reversed ? -1 : 1
      let n = typeof list.start === 'number' && list.hasAttribute('start') ? list.start : step < 0 ? items.length : 1
      for (const item of items) {
        if (item.hasAttribute('value')) n = item.value
        known.set(item, n)
        n += step
      }
      ordinals.set(list, known)
    }
    return known.has(el) ? known.get(el) + '.' : ''
  }

  // ---- click scan ---------------------------------------------------------

  // Natively interactive tags already surface through their role.
  const NATIVE = new Set(['A', 'BUTTON', 'INPUT', 'SELECT', 'TEXTAREA', 'SUMMARY', 'OPTION', 'LABEL'])
  // contextmenu / dblclick / dragstart carry no click listener and no pointer
  // cursor, so a target that only answers a right-click, a double-click or a
  // drag got no ref at all — it was not merely unclickable, it was invisible.
  const EVENTS = ['click', 'mousedown', 'pointerdown', 'keydown', 'contextmenu', 'dblclick', 'dragstart', 'dragover', 'drop']

  const isClickTarget = (el, cs, parentCursor) => {
    try {
      // Console-API only; most engines do not expose it to evaluated code.
      const ls = typeof getEventListeners === 'function' ? getEventListeners(el) : null
      if (ls && EVENTS.some((e) => ls[e] && ls[e].length)) return true
    } catch (e) { /* command line API unavailable */ }

    // What getEventListeners would have listed for a handler set as an attribute or a
    // property. Listeners added with addEventListener stay out of reach from in-page code.
    if (EVENTS.some((e) => typeof el['on' + e] === 'function') || el.hasAttribute('onclick')) return true

    const ti = el.getAttribute('tabindex')
    if (ti !== null && Number(ti) >= 0) return true

    // The cursor property INHERITS, so every descendant of a pointer element
    // reports 'pointer' too. Only the element that originates the change is
    // the real click target; without this test a single button contributes
    // one hit per nested span.
    return cs.cursor === 'pointer' && parentCursor !== 'pointer'
  }

  // ---- capture ------------------------------------------------------------

  // Only these actually scroll; `hidden` clips and `visible` spills.
  const SCROLLING_OVERFLOW = new Set(['auto', 'scroll', 'overlay'])
  const PRESERVES_SPACE = /^(pre|pre-wrap|break-spaces)$/

  function capture(opts) {
    const strings = []
    const stringIndex = new Map()
    const intern = (s) => {
      s = String(s)
      let i = stringIndex.get(s)
      if (i === undefined) {
        i = strings.length
        strings.push(s)
        stringIndex.set(s, i)
      }
      return i
    }

    // The flat node table and layout rows of a DOMSnapshot document. Node 0 is the
    // document; a same-origin frame's elements hang under their <iframe>, so a frame's
    // box clips its content the way a scroll container does.
    const dom = { backendNodeId: [idOf(document)], parentIndex: [-1], nodeType: [9] }
    const lay = { nodeIndex: [], styles: [], bounds: [], paintOrders: [], scrollRects: [], clientRects: [] }
    const layers = []
    const frames = []
    const last = new Map()
    const hits = []
    const paths = new Map()
    const ordinals = new Map()
    const unlisted = []
    let mainCount = 0
    let textSeq = 0

    const rootNode = (doc) => ({
      nodeId: String(idOf(doc)),
      backendDOMNodeId: idOf(doc),
      role: { value: 'RootWebArea' },
      name: { value: String(doc.title || '') },
      childIds: []
    })

    const enterFrame = (el, id, child, cs) => {
      let doc = null
      try {
        doc = el.contentDocument
      } catch (e) { /* cross-origin */ }
      // A cross-origin document cannot be read from here. The frame keeps its bare
      // Iframe line in the outline and is counted there as not entered.
      if (!doc || !doc.documentElement) return false
      if (child.frameDepth >= opts.maxFrameDepth || frames.length >= opts.maxFrames) return false

      const r = el.getBoundingClientRect()
      const frame = { frameId: 'f' + id, ownerBackendNodeId: id, url: String(doc.URL || ''), nodes: [rootNode(doc)] }
      frames.push(frame)
      child.ax = frame.nodes
      child.frameDepth++
      // The child document starts at the content box, not the border box.
      child.offX += r.left + num(cs.borderLeftWidth) + num(cs.paddingLeft)
      child.offY += r.top + num(cs.borderTopWidth) + num(cs.paddingTop)
      // The click scan stays in the main document, as its paths do.
      child.main = false
      child.shadowDepth = 0
      child.cursor = ''
      visit(doc.documentElement, child, frame.nodes[0].childIds)
      return true
    }

    // axOut: where this element's accessibility node id goes, or null inside a subtree
    // that has no accessibility nodes (aria-hidden, svg internals) but still has boxes.
    const visit = (el, ctx, axOut) => {
      const tag = el.tagName.toUpperCase()
      if (SKIP_TAGS.has(tag)) return
      const cs = styleOf(el)
      // display:none has no box and no accessibility node, and neither has anything under it.
      if (!cs || cs.display === 'none') return

      const id = idOf(el)
      const idx = dom.backendNodeId.length
      dom.backendNodeId.push(id)
      dom.parentIndex.push(ctx.parentIdx)
      dom.nodeType.push(1)
      last.set(id, el)

      // Element-child index path from <html>, counted the way buildElementPaths counts
      // it from the node table above, so the two always agree.
      const k = ctx.count.n++
      const path = ctx.path === null ? '' : (ctx.path === '' ? '' : ctx.path + '.') + k
      const isFrame = tag === 'IFRAME' || tag === 'FRAME'

      // shortcut: paint order is approximated as "positioned boxes above the flow,
      // ordered by z-index, descendants above their positioned ancestor"; nested
      // stacking contexts are flattened. Upgrade if an engine exposes real paint order.
      let layer = ctx.layer
      if (cs.position !== 'static') {
        const z = parseInt(cs.zIndex, 10)
        const own = isNaN(z) ? 1 : z < 0 ? -1 : z + 1
        layer = own < 0 ? own : Math.max(layer, own)
      }

      // A layout row only for an element that generated a box. display:contents has
      // none while its children do, and neither has an option of a closed select;
      // a row of zeros for those would count them as hidden controls.
      if (el.getClientRects().length > 0) {
        const r = el.getBoundingClientRect()
        // An iframe clips its document like overflow does.
        const ox = isFrame ? 'clip' : cs.overflowX
        const oy = isFrame ? 'clip' : cs.overflowY
        const scrolls = SCROLLING_OVERFLOW.has(ox) || SCROLLING_OVERFLOW.has(oy)
        lay.nodeIndex.push(idx)
        lay.bounds.push([r.left + ctx.offX, r.top + ctx.offY, r.width, r.height])
        lay.styles.push([
          intern(cs.visibility), intern(cs.opacity), intern(ox), intern(oy),
          intern(cs.position), intern(cs.backgroundColor)
        ])
        // scrollRects/clientRects: the pair that says whether a box really scrolls
        lay.scrollRects.push(scrolls ? [0, 0, el.scrollWidth, el.scrollHeight] : [])
        lay.clientRects.push(scrolls ? [0, 0, el.clientWidth, el.clientHeight] : [])
        layers.push(layer)
      }

      if (ctx.main && ctx.shadowDepth <= opts.maxShadowDepth) {
        mainCount++
        if (!NATIVE.has(tag) && isClickTarget(el, cs, ctx.cursor)) {
          hits.push(el)
          paths.set(el, path)
        }
      }

      const shown = cs.visibility === 'visible'
      const quiet = tag === 'LABEL' && redundantLabel(el)
      let childOut = null
      // aria-hidden removes the element and everything under it from the tree.
      if (axOut && el.getAttribute('aria-hidden') !== 'true') {
        const role = roleOf(el, tag)
        const node = { nodeId: String(id), backendDOMNodeId: id, role: { value: role }, childIds: [] }
        // visibility can be turned back on by a descendant, so the node stays as a
        // silent container instead of taking its subtree with it.
        if (shown && !quiet) describe(el, tag, role, node)
        else node.ignored = true
        // Chrome leaves out a canvas that has neither a name nor fallback content.
        if (tag === 'CANVAS' && !node.name && !el.children.length && !/\S/.test(el.textContent || '')) {
          node.ignored = true
        }
        ctx.ax.push(node)
        axOut.push(node.nodeId)
        // An svg reads as one image; its shapes are still walked for boxes and click targets.
        childOut = tag === 'SVG' ? null : node.childIds

        // Three things Chrome's tree has that the DOM does not: a list item's marker,
        // the text typed into a field as the field's own child, and the popup that
        // holds a closed select's options. All are added so the outline reads the same.
        const marker = shown && tag === 'LI' ? listMarker(el, cs, ordinals) : ''
        if (marker) {
          const mid = 't' + ++textSeq
          ctx.ax.push({ nodeId: mid, role: { value: 'ListMarker' }, name: { value: marker } })
          node.childIds.push(mid)
        }
        const typed = tag === 'TEXTAREA' || (tag === 'INPUT' && TEXT_INPUTS.has(inputType(el)))
        // One text per typed line, as Chrome splits a textarea.
        for (const line of typed && node.value ? node.value.value.split('\n') : []) {
          const tid = 't' + ++textSeq
          ctx.ax.push({ nodeId: tid, role: { value: 'StaticText' }, name: { value: line } })
          node.childIds.push(tid)
        }
        if (tag === 'TEXTAREA') childOut = null
        if (tag === 'SELECT' && role === 'combobox') {
          const popup = { nodeId: 'p' + id, role: { value: 'MenuListPopup' }, childIds: [] }
          ctx.ax.push(popup)
          node.childIds.push(popup.nodeId)
          childOut = popup.childIds
        }
      }

      const child = {
        ax: ctx.ax, parentIdx: idx, path: path, count: { n: 0 }, offX: ctx.offX, offY: ctx.offY,
        shadowDepth: ctx.shadowDepth + (el.shadowRoot ? 1 : 0), frameDepth: ctx.frameDepth,
        cursor: cs.cursor, layer: layer, main: ctx.main
      }
      if (isFrame) {
        // A frame that could not be read and has no line of its own in the outline
        // (it sits under aria-hidden) would otherwise vanish without a word.
        if (!enterFrame(el, id, child, cs) && childOut === null) {
          unlisted.push({ frameId: 'f' + id, url: String(el.src || '') })
        }
        return
      }
      // A closed <details> renders its summary and nothing else.
      const kids = tag === 'DETAILS' && !el.open ? [firstChild(el, 'SUMMARY')].filter(Boolean) : flatKids(el)
      for (let i = 0; i < kids.length; i++) {
        const c = kids[i]
        if (c.nodeType === 1) visit(c, child, childOut)
        else if (c.nodeType === 3 && childOut && shown && !quiet && /[^ \t\n\r\f]/.test(c.data)) {
          const tid = 't' + ++textSeq
          // Text reads as rendered: whitespace collapses unless the element preserves it.
          const text = PRESERVES_SPACE.test(cs.whiteSpace) ? c.data : squash(c.data)
          ctx.ax.push({ nodeId: tid, role: { value: 'StaticText' }, name: { value: text } })
          childOut.push(tid)
        }
      }
    }

    const nodes = [rootNode(document)]
    if (document.documentElement) {
      visit(
        document.documentElement,
        {
          ax: nodes, parentIdx: 0, path: null, count: { n: 0 }, offX: window.scrollX, offY: window.scrollY,
          shadowDepth: 0, frameDepth: 0, cursor: '', layer: 0, main: true
        },
        nodes[0].childIds
      )
    }

    const rows = layers.length + 1
    for (let i = 0; i < layers.length; i++) lay.paintOrders.push(layers[i] * rows + i)

    let click
    if (mainCount > opts.maxScannedElements) {
      click = { paths: [], scanned: mainCount, skipped: true }
    } else {
      // Innermost candidate wins. A delegation root (React 17+ attaches ONE
      // listener at the app container) is itself a hit, and preferring the outer
      // element would collapse an entire app into a single ref and hide every
      // button inside it. Nested inheritance is already handled by the cursor
      // origin test above, so nothing needs the outer element to win.
      const hitSet = new Set(hits)
      const hasInnerHit = new Set()
      for (const el of hits) {
        let p = el.parentElement
        while (p) {
          if (hitSet.has(p)) hasInnerHit.add(p)
          p = p.parentElement
        }
      }
      const out = []
      for (const el of hits) {
        if (hasInnerHit.has(el)) continue
        const path = paths.get(el)
        if (path !== '') out.push(path)
      }
      click = { paths: out, scanned: mainCount, skipped: false }
    }

    state.last = last
    return {
      meta: {
        url: String(location.href), title: String(document.title || ''),
        scrollX: window.scrollX, scrollY: window.scrollY,
        innerWidth: window.innerWidth, innerHeight: window.innerHeight
      },
      nodes: nodes,
      frames: frames,
      unlistedFrames: unlisted,
      layout: { documents: [{ nodes: dom, layout: lay }], strings: strings },
      click: click
    }
  }

  // ---- refs and actions ---------------------------------------------------

  // Keep the elements that got a ref, and let go of every other element of the capture.
  function setRefs(map) {
    const refs = new Map()
    for (const ref of Object.keys(map)) {
      const el = state.last && state.last.get(map[ref])
      if (el) refs.set(ref, el)
    }
    state.refs = refs
    state.last = null
    return refs.size
  }

  // Where a frame's content starts in the top viewport, measured now.
  //
  // Coordinates from getBoundingClientRect inside a frame are relative to THAT
  // frame, while mouse events are dispatched in page space, so the frames'
  // positions have to be added back. They are measured at click time rather
  // than stored from the snapshot: scrollIntoView moves the page under the
  // frame, and a stored offset is wrong by exactly that scroll. The symptom is
  // not an error — the click lands on whatever is now at those coordinates.
  const frameOffset = (el) => {
    let x = 0
    let y = 0
    let view = viewOf(el)
    while (view && view !== window) {
      let owner = null
      try {
        owner = view.frameElement
      } catch (e) { /* cross-origin parent */ }
      if (!owner) break
      const r = owner.getBoundingClientRect()
      const s = styleOf(owner)
      x += r.left + num(s.borderLeftWidth) + num(s.paddingLeft)
      y += r.top + num(s.borderTopWidth) + num(s.paddingTop)
      view = viewOf(owner)
    }
    return { x: x, y: y }
  }

  // Bring the element into view before measuring it. It only scrolls when the
  // element is actually outside the viewport — centring on every click made the
  // page jump on targets that were already visible. The jump is instant, because
  // the caller measures straight afterwards and a still-animating page yields a
  // stale point.
  const scrollIntoViewIfNeeded = (el) => {
    const margin = 8
    const outside = (r, view) =>
      r.top < margin || r.left < margin ||
      r.bottom > view.innerHeight - margin || r.right > view.innerWidth - margin
    const r = el.getBoundingClientRect()
    const off = frameOffset(el)
    const inPage = { top: r.top + off.y, left: r.left + off.x, bottom: r.bottom + off.y, right: r.right + off.x }
    if (outside(r, viewOf(el)) || outside(inPage, window)) {
      el.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' })
    }
  }

  function act(a) {
    let el
    if (a.selector != null) {
      try {
        el = document.querySelector(a.selector)
      } catch (e) { /* an invalid selector matches nothing */ }
      if (!el) return { error: 'no element matches selector: ' + a.selector }
    } else {
      el = state.refs.get(a.ref)
      // The ref can outlive its element: the page re-rendered or navigated since the snapshot.
      if (!el || !el.isConnected) {
        return { error: 'ref "' + a.ref + '" is no longer on the page — call browser_snapshot again' }
      }
    }
    const tag = el.tagName.toUpperCase()

    switch (a.action) {
      case 'point': {
        scrollIntoViewIfNeeded(el)
        const r = el.getBoundingClientRect()
        const off = frameOffset(el)
        return {
          ok: {
            x: r.left + r.width / 2 + off.x, y: r.top + r.height / 2 + off.y,
            left: r.left + off.x, top: r.top + off.y, width: r.width, height: r.height
          }
        }
      }
      case 'focus':
        if (el.focus) el.focus()
        return { ok: true }
      case 'setValue': {
        const view = viewOf(el)
        const fire = (type) => el.dispatchEvent(new view.Event(type, { bubbles: true }))
        if (tag === 'INPUT' || tag === 'TEXTAREA') {
          if (el.focus) el.focus()
          // Through the prototype's setter: frameworks that track the value (React)
          // replace the instance property and would not see a plain assignment.
          const proto = view[tag === 'INPUT' ? 'HTMLInputElement' : 'HTMLTextAreaElement']
          const desc = proto && Object.getOwnPropertyDescriptor(proto.prototype, 'value')
          if (desc && desc.set) desc.set.call(el, a.value)
          else el.value = a.value
        } else if (el.isContentEditable) {
          if (el.focus) el.focus()
          el.textContent = a.value
        } else {
          return { error: 'ref is a <' + tag.toLowerCase() + '>, not a text field' }
        }
        fire('input')
        fire('change')
        return { ok: true }
      }
      case 'text': {
        if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return { ok: String(el.value == null ? '' : el.value) }
        const text = el.innerText != null ? el.innerText : el.textContent
        return { ok: String(text == null ? '' : text) }
      }
      case 'select': {
        if (tag !== 'SELECT') return { error: 'ref is a <' + tag.toLowerCase() + '>, not a <select>' }
        const wanted = new Set(a.value)
        const picked = []
        for (const opt of el.options) {
          const label = (opt.label || opt.text || '').trim()
          const on = wanted.has(opt.value) || wanted.has(label)
          opt.selected = on
          if (on) picked.push(label || opt.value)
        }
        if (picked.length === 0) {
          return {
            error: 'no option matched',
            options: Array.from(el.options).map((o) => (o.label || o.text || '').trim())
          }
        }
        const view = viewOf(el)
        el.dispatchEvent(new view.Event('input', { bubbles: true }))
        el.dispatchEvent(new view.Event('change', { bubbles: true }))
        return { ok: picked }
      }
      case 'scrollState':
        return { ok: { canScroll: el.scrollHeight > el.clientHeight, scrollTop: el.scrollTop } }
    }
    return { error: 'unknown action: ' + a.action }
  }

  const out = command === 'capture' ? capture(arg) : command === 'setRefs' ? setRefs(arg) : act(arg)
  return JSON.stringify(out)
}
