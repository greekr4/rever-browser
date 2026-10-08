// Pure helpers for inspect_listeners (electron-free, vitest-testable).

// The CDP DOMDebugger.EventListener fields this tool reads.
export interface RawListener {
  type: string
  useCapture: boolean
  passive: boolean
  once: boolean
  scriptId: string
  lineNumber: number
  columnNumber: number
}

export interface ListenerOwner {
  /** 0 = the selected element, n = n levels up (document / window last). */
  depth: number
  label: string
}

export interface ListenerInfo {
  type: string
  on: string
  capture: boolean
  passive: boolean
  once: boolean
  /** 1-based `url:line:col`, or `script#<id>:line:col` when the URL is unknown. */
  location: string
  url: string | null
  line: number
  column: number
}

export const LISTENER_LIMITATIONS = [
  'Only listeners registered with addEventListener / on* properties are listed; framework registries (React props, Vue handlers) are not read directly — with ancestors on, their root-level dispatcher shows up instead.',
  'Closed shadow roots and listeners added after this call are not covered.',
  'Handlers are never invoked; source is the function text, not proof it runs on click.'
]

export function nodeLabel(nodeName: string, attributes: string[]): string {
  if (nodeName === '#document') return 'document'
  if (nodeName === 'window') return 'window'
  let label = nodeName.toLowerCase()
  for (let i = 0; i + 1 < attributes.length; i += 2) {
    const [name, value] = [attributes[i], attributes[i + 1]]
    if (name === 'id' && value) label += `#${value}`
  }
  for (let i = 0; i + 1 < attributes.length; i += 2) {
    if (attributes[i] !== 'class') continue
    // Utility-class frameworks put a dozen classes on one node; three is
    // enough to recognise it.
    const classes = attributes[i + 1].split(/\s+/).filter(Boolean)
    for (const cls of classes.slice(0, 3)) label += `.${cls}`
    if (classes.length > 3) label += '…'
  }
  return label
}

// Source around a 0-based (line, column). Multi-line sources return whole lines
// from `line`; a minified bundle's single huge line returns a character window
// starting at the column instead, marked with … where it was cut.
export function snippetAt(
  source: string,
  line: number,
  column: number,
  opts: { maxLines?: number; maxChars?: number } = {}
): string {
  const maxLines = opts.maxLines ?? 40
  const maxChars = opts.maxChars ?? 1_500
  const lines = source.split('\n')
  if (line < 0 || line >= lines.length) return ''
  const target = lines[line]
  if (target.length > maxChars) {
    const start = Math.min(Math.max(0, column), target.length)
    const end = Math.min(target.length, start + maxChars)
    return `${start > 0 ? '…' : ''}${target.slice(start, end)}${end < target.length ? '…' : ''}`
  }
  const out = lines.slice(line, line + maxLines).join('\n')
  return out.length > maxChars ? `${out.slice(0, maxChars)}…` : out
}

export function normalizeListener(
  raw: RawListener,
  owner: ListenerOwner,
  url: string | null
): ListenerInfo {
  const line = raw.lineNumber + 1
  const column = raw.columnNumber + 1
  return {
    type: raw.type,
    on: owner.depth === 0 ? `self (${owner.label})` : `ancestor +${owner.depth} (${owner.label})`,
    capture: raw.useCapture,
    passive: raw.passive,
    once: raw.once,
    location: `${url ?? `script#${raw.scriptId}`}:${line}:${column}`,
    url,
    line,
    column
  }
}
