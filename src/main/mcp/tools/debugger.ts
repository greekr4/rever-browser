import { z } from 'zod'
import { randomUUID } from 'node:crypto'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import {
  getActiveTarget,
  getDebuggerPaused,
  getBreakpoints,
  addBreakpoint,
  removeBreakpoint
} from '../../chrome-cdp'
import { ok, err, errorMessage } from '../utils'

function globToRegex(glob: string): string {
  return glob
    .replace(/[.+^${}()|[\]\\]/g, '\\$&')
    .replace(/\*/g, '.*')
    .replace(/\?/g, '.')
}

export function registerDebuggerTools(mcp: McpServer) {
  mcp.registerTool(
    'bp_add',
    {
      description:
        'Set a breakpoint at a URL glob + line number. The page will pause when execution reaches the location.',
      inputSchema: {
        urlGlob: z.string().describe('URL glob pattern, e.g. "*/main.*.js"'),
        line: z.number().int().nonnegative().describe('0-indexed line number'),
        column: z.number().int().nonnegative().optional().describe('0-indexed column number'),
        condition: z.string().optional().describe('Conditional expression (JS); only pause when truthy')
      }
    },
    async ({ urlGlob, line, column, condition }) => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        const res = (await target.dbg.sendCommand('Debugger.setBreakpointByUrl', {
          urlRegex: globToRegex(urlGlob),
          lineNumber: line,
          ...(column != null ? { columnNumber: column } : {}),
          ...(condition ? { condition } : {})
        })) as { breakpointId: string; locations: unknown[] }

        const id = randomUUID()
        addBreakpoint({ id, breakpointId: res.breakpointId, urlGlob, line, column, condition })
        return ok(JSON.stringify({ id, breakpointId: res.breakpointId, locations: res.locations }, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'bp_list',
    {
      description: 'List all active breakpoints.'
    },
    async () => {
      return ok(JSON.stringify(getBreakpoints(), null, 2))
    }
  )

  mcp.registerTool(
    'bp_remove',
    {
      description: 'Remove a breakpoint by its short ID (from bp_list).',
      inputSchema: {
        id: z.string().describe('Short breakpoint ID from bp_list')
      }
    },
    async ({ id }) => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      const bp = removeBreakpoint(id)
      if (!bp) return err(`unknown breakpoint id: ${id}`)
      try {
        await target.dbg.sendCommand('Debugger.removeBreakpoint', { breakpointId: bp.breakpointId })
        return ok(`breakpoint ${id} removed`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'bp_status',
    {
      description: 'Return the current debugger pause state. If paused, shows the top call frames.',
    },
    async () => {
      const paused = getDebuggerPaused()
      if (!paused) return ok(JSON.stringify({ paused: false }, null, 2))

      const frames = paused.callFrames.slice(0, 5).map((f) => ({
        functionName: f.functionName || '(anonymous)',
        location: f.location,
        scopeKeys: f.scopeChain.map((s) => `${s.type}: ${s.object.description ?? '?'}`)
      }))
      return ok(JSON.stringify({ paused: true, reason: paused.reason, frames }, null, 2))
    }
  )

  mcp.registerTool(
    'bp_resume',
    {
      description: 'Resume execution after a breakpoint pause.'
    },
    async () => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        await target.dbg.sendCommand('Debugger.resume')
        return ok('resumed')
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'bp_step_over',
    {
      description: 'Step over the current line (must be paused at a breakpoint).'
    },
    async () => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        await target.dbg.sendCommand('Debugger.stepOver')
        return ok('stepped over')
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'bp_step_into',
    {
      description: 'Step into the next function call (must be paused at a breakpoint).'
    },
    async () => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        await target.dbg.sendCommand('Debugger.stepInto')
        return ok('stepped into')
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'bp_step_out',
    {
      description: 'Step out of the current function (must be paused at a breakpoint).'
    },
    async () => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      try {
        await target.dbg.sendCommand('Debugger.stepOut')
        return ok('stepped out')
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'bp_eval_in_frame',
    {
      description:
        'Evaluate a JavaScript expression in a specific call frame while paused at a breakpoint.',
      inputSchema: {
        frameIndex: z.number().int().nonnegative().describe('Index into the paused call frames (0 = top frame)'),
        expression: z.string().describe('JavaScript expression to evaluate in that frame')
      }
    },
    async ({ frameIndex, expression }) => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target')
      const paused = getDebuggerPaused()
      if (!paused) return err('debugger is not paused — set a breakpoint first')
      const frame = paused.callFrames[frameIndex]
      if (!frame) return err(`no frame at index ${frameIndex} (${paused.callFrames.length} frames total)`)
      try {
        const res = (await target.dbg.sendCommand('Debugger.evaluateOnCallFrame', {
          callFrameId: frame.callFrameId,
          expression,
          returnByValue: true,
          generatePreview: false
        })) as { result: { value?: unknown; description?: string }; exceptionDetails?: { text: string } }

        if (res.exceptionDetails) return err(res.exceptionDetails.text)
        const val = res.result.value !== undefined ? res.result.value : res.result.description
        return ok(JSON.stringify(val, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
