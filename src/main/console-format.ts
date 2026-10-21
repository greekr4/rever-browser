// Electron-free formatting of a CDP Runtime.evaluate reply for the console
// panel's command line, close to how Chrome's console prints values.

interface PropertyPreview {
  name: string
  type: string
  value?: string
  subtype?: string
}

interface RemoteObject {
  type: string
  subtype?: string
  value?: unknown
  unserializableValue?: string
  description?: string
  preview?: { type: string; subtype?: string; overflow: boolean; properties: PropertyPreview[] }
}

export interface EvalReply {
  result: RemoteObject
  exceptionDetails?: { text: string; exception?: RemoteObject }
}

function formatPreviewValue(p: PropertyPreview): string {
  return p.type === 'string' ? JSON.stringify(p.value ?? '') : (p.value ?? p.type)
}

function formatObject(o: RemoteObject): string {
  if (o.subtype === 'null') return 'null'
  const pv = o.preview
  if (!pv) return o.description ?? 'Object'
  const more = pv.overflow ? ', …' : ''
  if (pv.subtype === 'array') return `[${pv.properties.map(formatPreviewValue).join(', ')}${more}]`
  // Errors, DOM nodes, functions etc. read better as their description.
  if (pv.subtype || o.subtype) return o.description ?? 'Object'
  const body = pv.properties.map((p) => `${p.name}: ${formatPreviewValue(p)}`).join(', ')
  const name = o.description && o.description !== 'Object' ? `${o.description} ` : ''
  return `${name}{${body}${more}}`
}

function formatRemoteObject(o: RemoteObject): string {
  switch (o.type) {
    case 'undefined':
      return 'undefined'
    case 'string':
      return JSON.stringify(o.value)
    case 'object':
      return formatObject(o)
    default:
      return o.unserializableValue ?? o.description ?? String(o.value)
  }
}

export function formatEvalResult(r: EvalReply): { text: string; isError: boolean } {
  if (r.exceptionDetails) {
    const ex = r.exceptionDetails.exception
    const desc = ex ? (ex.description ?? formatRemoteObject(ex)) : ''
    return { text: `${r.exceptionDetails.text} ${desc}`.trim(), isError: true }
  }
  return { text: formatRemoteObject(r.result), isError: false }
}
