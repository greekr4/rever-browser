import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getActiveTarget } from '../../chrome-cdp'
import { scriptUrl } from '../../script-registry'
import { listRequests } from '../../traffic-store'
import {
  LISTENER_LIMITATIONS,
  nodeLabel,
  normalizeListener,
  snippetAt,
  type RawListener
} from '../listeners'
import { resolveRefObject, resolveSelectorObjectId } from '../snapshot'
import { okBudgeted, err, errorMessage } from '../utils'

// Every remote object this tool creates goes in one group, released at the end.
const GROUP = 'rever-inspect-listeners'

type Send = (method: string, params?: object) => Promise<unknown>

interface RemoteObject {
  objectId?: string
}

// The element, then (optionally) every ancestor up through shadow hosts to
// document and window — where React/Vue-style delegated handlers live.
const CHAIN_FN = `function(withAncestors) {
  const out = [this]
  if (withAncestors) {
    let n = this
    while (n) {
      n = n.parentNode || n.host || null
      if (n) out.push(n)
    }
    out.push(window)
  }
  return out
}`

const LABEL_FN = `function() {
  return this.map((n) => {
    if (n === window) return { nodeName: 'window', attrs: [] }
    const attrs = []
    if (n.attributes) for (const a of n.attributes) attrs.push(a.name, a.value)
    return { nodeName: n.nodeName, attrs }
  })
}`

const TO_STRING_FN = `function() {
  try { return Function.prototype.toString.call(this) } catch (e) { return '' }
}`

async function chainOf(
  send: Send,
  objectId: string,
  ancestors: boolean
): Promise<Array<{ objectId: string; label: string }>> {
  const arr = (await send('Runtime.callFunctionOn', {
    objectId,
    functionDeclaration: CHAIN_FN,
    arguments: [{ value: ancestors }],
    objectGroup: GROUP
  })) as { result: RemoteObject }
  const arrayId = arr.result.objectId
  if (!arrayId) return []
  const labels = (await send('Runtime.callFunctionOn', {
    objectId: arrayId,
    functionDeclaration: LABEL_FN,
    returnByValue: true
  })) as { result: { value: Array<{ nodeName: string; attrs: string[] }> } }
  const props = (await send('Runtime.getProperties', {
    objectId: arrayId,
    ownProperties: true
  })) as { result: Array<{ name: string; value?: RemoteObject }> }
  const out: Array<{ objectId: string; label: string }> = []
  for (const p of props.result) {
    const i = Number(p.name)
    if (!Number.isInteger(i) || !p.value?.objectId) continue
    const d = labels.result.value[i]
    out[i] = { objectId: p.value.objectId, label: d ? nodeLabel(d.nodeName, d.attrs) : '?' }
  }
  return out.filter(Boolean)
}

async function handlerSource(
  send: Send,
  raw: RawListener & { handler?: RemoteObject },
  maxChars: number
): Promise<string> {
  if (raw.handler?.objectId) {
    const res = (await send('Runtime.callFunctionOn', {
      objectId: raw.handler.objectId,
      functionDeclaration: TO_STRING_FN,
      returnByValue: true
    }).catch(() => null)) as { result?: { value?: string } } | null
    const text = res?.result?.value
    if (text) return snippetAt(text, 0, 0, { maxChars })
  }
  const src = (await send('Debugger.getScriptSource', { scriptId: raw.scriptId }).catch(
    () => null
  )) as { scriptSource?: string } | null
  return src?.scriptSource ? snippetAt(src.scriptSource, raw.lineNumber, raw.columnNumber, { maxChars }) : ''
}

export function registerListenerTools(mcp: McpServer) {
  mcp.registerTool(
    'inspect_listeners',
    {
      description:
        'List the event listeners on an element — type, flags, `url:line:col` of the handler, and its source. Use it to go button → handler → request before clicking. ancestors=true (default) also walks parents up to document/window, where React/Vue-style frameworks register their delegated handlers. Pass the `resolveSource` object of a listener to resolve_source for the original (source-mapped) location. Never invokes a handler.',
      inputSchema: {
        ref: z.string().optional().describe('Element ref from browser_snapshot (e.g. "r12")'),
        selector: z.string().optional().describe('CSS selector (first match, main document)'),
        ancestors: z.boolean().optional().describe('Also list listeners on ancestors, document and window (default true)'),
        types: z
          .array(z.string())
          .optional()
          .describe('Only these event types, e.g. ["click","submit"] — cuts window-level noise'),
        maxSourceChars: z.number().int().positive().max(10_000).optional().describe('Per-handler source cap (default 1500)')
      }
    },
    async ({ ref, selector, ancestors = true, types, maxSourceChars = 1_500 }) => {
      if (!ref === !selector) return err('pass exactly one of ref or selector')
      const target = getActiveTarget()
      if (!target) return err('no active browser target')

      let objectId: string
      let sessionId: string | undefined
      try {
        if (ref) ({ objectId, sessionId } = await resolveRefObject(ref))
        else objectId = await resolveSelectorObjectId(selector as string)
      } catch (e) {
        return err(errorMessage(e))
      }
      const send: Send = (method, params) => target.dbg.sendCommand(method, params, sessionId)

      try {
        const chain = await chainOf(send, objectId, ancestors)
        const wanted = types?.length ? new Set(types) : null
        const scripts = listRequests({ methodOrType: 'Script' })
        const listeners = []
        for (const [depth, node] of chain.entries()) {
          const res = (await send('DOMDebugger.getEventListeners', {
            objectId: node.objectId,
            depth: 0
          })) as { listeners: Array<RawListener & { handler?: RemoteObject }> }
          for (const raw of res.listeners) {
            if (wanted && !wanted.has(raw.type)) continue
            const url = scriptUrl(target.wc.id, sessionId, raw.scriptId)
            const scriptRequestId = url ? (scripts.find((r) => r.url === url)?.requestId ?? null) : null
            listeners.push({
              ...normalizeListener(raw, { depth, label: node.label }, url),
              // Ready-made resolve_source arguments (it takes 0-based positions).
              resolveSource: scriptRequestId
                ? { requestId: scriptRequestId, line: raw.lineNumber, column: raw.columnNumber }
                : null,
              source: await handlerSource(send, raw, maxSourceChars)
            })
          }
        }
        return okBudgeted(
          JSON.stringify(
            {
              element: chain[0]?.label ?? null,
              searched: chain.map((n) => n.label),
              count: listeners.length,
              listeners,
              limitations: LISTENER_LIMITATIONS
            },
            null,
            2
          )
        )
      } catch (e) {
        return err(errorMessage(e))
      } finally {
        await send('Runtime.releaseObjectGroup', { objectGroup: GROUP }).catch(() => null)
      }
    }
  )
}
