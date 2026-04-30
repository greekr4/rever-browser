import { emitAiAction } from '../ai-events'
import { getActiveTarget } from '../chrome-cdp'
import { humanMouseMove, humanPressRelease, humanType, thinkingPause } from './human-input'

interface AXValue {
  type: string
  value?: unknown
}

interface AXProperty {
  name: string
  value: AXValue
}

interface AXNode {
  nodeId: string
  ignored?: boolean
  childIds?: string[]
  role?: AXValue
  name?: AXValue
  value?: AXValue
  properties?: AXProperty[]
  backendDOMNodeId?: number
}

interface RefEntry {
  backendNodeId: number
  role: string
  name: string
}

const refMap = new Map<string, RefEntry>()

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
  'option'
])

function quote(s: string): string {
  return JSON.stringify(s.length > 80 ? s.slice(0, 80) + '…' : s)
}

export interface SnapshotResult {
  url: string
  title: string
  tree: string
}

export async function takeSnapshot(): Promise<SnapshotResult> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target — open a page first')
  refMap.clear()

  await target.dbg.sendCommand('Accessibility.enable').catch(() => {})

  const meta = (await target.dbg.sendCommand('Runtime.evaluate', {
    expression: '({ url: location.href, title: document.title })',
    returnByValue: true
  })) as { result: { value: { url: string; title: string } } }

  const { nodes } = (await target.dbg.sendCommand('Accessibility.getFullAXTree')) as {
    nodes: AXNode[]
  }

  const byId = new Map<string, AXNode>()
  for (const n of nodes) byId.set(n.nodeId, n)

  let counter = 0
  const lines: string[] = []

  const walk = (id: string, depth: number, parentName: string): void => {
    const n = byId.get(id)
    if (!n) return
    const role = (n.role?.value as string | undefined) ?? ''
    const name = String((n.name?.value as string | undefined) ?? '').trim()

    // StaticText is collapsed into its parent's name. Suppress entirely if it
    // duplicates the parent name (Playwright MCP rule); otherwise emit as a
    // single quoted text line without a ref (text is not actionable).
    if (role === 'StaticText') {
      if (!name || name === parentName) return
      lines.push(`${'  '.repeat(depth)}- text ${quote(name)}`)
      return
    }

    const skip = !role || n.ignored || (SKIP_ROLES.has(role) && !name)
    let nextDepth = depth

    if (!skip) {
      const parts: string[] = [`- ${role}`]
      if (name) parts.push(quote(name))

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

      if (n.backendDOMNodeId != null && ACTIONABLE_ROLES.has(role)) {
        counter++
        const r = `r${counter}`
        refMap.set(r, { backendNodeId: n.backendDOMNodeId, role, name })
        parts.push(`[ref=${r}]`)
      }

      lines.push(`${'  '.repeat(depth)}${parts.join(' ')}`)
      nextDepth = depth + 1
    }

    const childParentName = skip ? parentName : name
    for (const c of n.childIds ?? []) walk(c, nextDepth, childParentName)
  }

  if (nodes[0]) walk(nodes[0].nodeId, 0, '')

  return {
    url: meta.result.value.url,
    title: meta.result.value.title,
    tree: lines.join('\n')
  }
}

async function resolveObjectId(ref: string): Promise<string> {
  const entry = refMap.get(ref)
  if (!entry) throw new Error(`unknown ref "${ref}" — call browser_snapshot first`)
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target')
  const res = (await target.dbg.sendCommand('DOM.resolveNode', {
    backendNodeId: entry.backendNodeId
  })) as { object: { objectId: string } }
  return res.object.objectId
}

export async function clickRef(ref: string): Promise<void> {
  const entry = refMap.get(ref)
  const objectId = await resolveObjectId(ref)
  const target = getActiveTarget()!
  const label = `AI click${entry?.name ? ` "${entry.name.slice(0, 32)}"` : ''}`
  emitAiAction({ kind: 'click', label, detail: entry?.role })

  // 1. Scroll into view + return target center. No flash yet — overlay
  //    appears only once the cursor has actually arrived.
  const result = (await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: `async function() {
      this.scrollIntoView({block:"center"})
      await new Promise(r => requestAnimationFrame(() => r()))
      const r = this.getBoundingClientRect()
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 }
    }`,
    returnByValue: true,
    awaitPromise: true
  })) as { result: { value: { x: number; y: number } } }
  const { x, y } = result.result.value

  // 2. Human-shaped pause, then move the cursor to the target.
  await thinkingPause()
  await humanMouseMove(x, y)

  // 3. Cursor has arrived — NOW flash the highlight, then press/release.
  await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: `function(label) {
      if (window.__reverAi) window.__reverAi.flashElement(this, label, 'click')
    }`,
    arguments: [{ value: label }]
  })
  await humanPressRelease(x, y)
}

export async function typeRef(ref: string, text: string, submit: boolean): Promise<void> {
  const entry = refMap.get(ref)
  const objectId = await resolveObjectId(ref)
  const target = getActiveTarget()!
  const label = `AI type${entry?.name ? ` → "${entry.name.slice(0, 24)}"` : ''}`
  emitAiAction({ kind: 'type', label, detail: text.slice(0, 80) })

  // 1. Scroll into view, get coords (no flash yet).
  const result = (await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: `async function() {
      this.scrollIntoView({block:"center"})
      await new Promise(r => requestAnimationFrame(() => r()))
      const r = this.getBoundingClientRect()
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 }
    }`,
    returnByValue: true,
    awaitPromise: true
  })) as { result: { value: { x: number; y: number } } }
  const { x, y } = result.result.value

  // 2. Pause + move cursor.
  await thinkingPause()
  await humanMouseMove(x, y)

  // 3. Cursor arrived — flash highlight, then click to focus, then type.
  await target.dbg.sendCommand('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: `function(label) {
      if (window.__reverAi) window.__reverAi.flashElement(this, label, 'type')
    }`,
    arguments: [{ value: label }]
  })
  await humanPressRelease(x, y)
  await humanType(objectId, text, submit)
}
