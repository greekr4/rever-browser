import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getActiveTarget } from '../../chrome-cdp'
import { getConsoleSince, getExceptions, clearConsole } from '../../traffic-store'
import { ok, err, errorMessage } from '../utils'

// Object handle table — maps short handle names to CDP objectIds
const handleTable = new Map<string, string>()
let handleCounter = 0

function nextHandle(): string {
  return `@h${++handleCounter}`
}

interface RemoteObject {
  type: string
  subtype?: string
  objectId?: string
  value?: unknown
  description?: string
  preview?: {
    description?: string
    properties?: Array<{ name: string; value?: string; type?: string }>
  }
}

function formatRemoteObject(ro: RemoteObject): string {
  if (ro.preview?.description) return ro.preview.description.slice(0, 200)
  if (ro.description) return ro.description.slice(0, 200)
  if (ro.value !== undefined) return JSON.stringify(ro.value)
  return `[${ro.type}]`
}

export function registerConsoleTools(mcp: McpServer) {
  mcp.registerTool(
    'console_eval',
    {
      description:
        'Evaluate a JavaScript expression in the active page. Supports await. Returns a preview of the result; complex objects get a short handle (@hN) for further inspection with console_get_props.',
      inputSchema: {
        expression: z.string().describe('JavaScript expression to evaluate'),
        contextId: z.number().int().optional().describe('Optional execution context ID')
      }
    },
    async ({ expression, contextId }) => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target — open a page first')
      try {
        const res = (await target.dbg.sendCommand('Runtime.evaluate', {
          expression,
          awaitPromise: true,
          returnByValue: false,
          generatePreview: true,
          replMode: true,
          ...(contextId != null ? { contextId } : {})
        })) as { result: RemoteObject; exceptionDetails?: { text: string; exception?: { description?: string } } }

        if (res.exceptionDetails) {
          const desc =
            res.exceptionDetails.exception?.description ??
            res.exceptionDetails.text ??
            'eval failed'
          return err(desc)
        }

        const ro = res.result
        let text = formatRemoteObject(ro)

        if (ro.objectId) {
          const handle = nextHandle()
          handleTable.set(handle, ro.objectId)
          text = `${text}\n[handle: ${handle}]`
        }

        return ok(text)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'console_logs',
    {
      description: 'Return captured console.log/warn/error messages from the active page.',
      inputSchema: {
        since: z.number().optional().describe('Only include logs after this epoch ms'),
        limit: z.number().int().positive().max(500).optional().describe('Max items (default 100)')
      }
    },
    async ({ since, limit }) => {
      const logs = getConsoleSince(since)
      const sliced = logs.slice(-(limit ?? 100))
      return ok(JSON.stringify(sliced, null, 2))
    }
  )

  mcp.registerTool(
    'console_exceptions',
    {
      description: 'Return captured JavaScript runtime exceptions from the active page.',
      inputSchema: {
        limit: z.number().int().positive().max(200).optional().describe('Max items (default 50)')
      }
    },
    async ({ limit }) => {
      const excs = getExceptions()
      const sliced = excs.slice(-(limit ?? 50))
      return ok(JSON.stringify(sliced, null, 2))
    }
  )

  mcp.registerTool(
    'console_clear',
    {
      description: 'Clear the captured console log buffer and all object handles from previous console_eval calls.'
    },
    async () => {
      clearConsole()
      handleTable.clear()
      return ok('console logs cleared and handles reset')
    }
  )

  mcp.registerTool(
    'console_get_props',
    {
      description:
        'Inspect an object handle returned by console_eval. Returns up to 50 key/value pairs.',
      inputSchema: {
        handle: z.string().describe('Handle from console_eval, e.g. "@h1"'),
        ownProperties: z.boolean().optional().describe('Only own properties (default true)')
      }
    },
    async ({ handle, ownProperties = true }) => {
      const objectId = handleTable.get(handle)
      if (!objectId) return err(`unknown handle: ${handle}. Use console_eval to get a handle first.`)
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        const res = (await target.dbg.sendCommand('Runtime.getProperties', {
          objectId,
          ownProperties: ownProperties ?? true,
          generatePreview: true
        })) as { result: Array<{ name: string; value?: RemoteObject; enumerable: boolean }> }

        const props = res.result
          .filter((p) => p.enumerable)
          .slice(0, 50)
          .map((p) => ({
            name: p.name,
            value: p.value ? formatRemoteObject(p.value) : undefined
          }))
        return ok(JSON.stringify(props, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
