import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { askRenderer } from '../../renderer-bridge'
import { runWorkflow, type RunStep } from '../../workflow-executor'
import { listMcpTools } from '../bridge'
import { ok, err, errorMessage } from '../utils'

const stepSchema = z.object({
  tool: z.string().describe('MCP tool name, e.g. "browser_click"'),
  input: z
    .record(z.string(), z.unknown())
    .optional()
    .describe('Tool arguments. String values may contain {{var}} placeholders.'),
  waitFor: z.string().optional().describe('CSS selector to wait for before this step runs'),
  delay: z.number().optional().describe('Extra pause (ms) before this step runs — useful for pacing a demo'),
  waitTimeout: z.number().optional().describe('Max ms to wait for waitFor'),
  saveAs: z.string().optional().describe('Store this step result in the run vars under this name')
})

/** Reject unknown tool names up front — a bad name only fails at replay time. */
async function unknownTools(steps: Array<{ tool: string }>): Promise<string[]> {
  const known = new Set((await listMcpTools()).map((t) => t.name))
  return [...new Set(steps.map((s) => s.tool).filter((n) => !known.has(n)))]
}

async function bridge(op: string, payload: unknown) {
  return askRenderer(op, payload)
}

function json(v: unknown): string {
  return JSON.stringify(v, null, 2)
}

export function registerMacroTools(mcp: McpServer) {
  mcp.registerTool(
    'list_macros',
    {
      description:
        'List the macros saved in the Workflows panel (id, name, description, step count).',
      inputSchema: {}
    },
    async () => {
      try {
        return ok(json(await bridge('macro:list', {})))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'get_macro',
    {
      description: 'Read one saved macro in full, including every step and its variables.',
      inputSchema: { id: z.string().describe('Macro id from list_macros') }
    },
    async ({ id }) => {
      try {
        return ok(json(await bridge('macro:get', { id })))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'create_macro',
    {
      description:
        'Save a new macro: a named sequence of MCP tool calls replayed deterministically with no LLM in the loop. Use this to turn actions you just performed into a reusable macro — list the same tool calls, in order, with the arguments you used.',
      inputSchema: {
        name: z.string().describe('Short name shown in the Workflows panel'),
        description: z.string().optional().describe('What this macro does'),
        steps: z.array(stepSchema).describe('Tool calls in execution order'),
        vars: z
          .string()
          .optional()
          .describe('JSON object (as text) supplying values for {{var}} placeholders')
      }
    },
    async ({ name, description, steps, vars }) => {
      const bad = await unknownTools(steps)
      if (bad.length) return err(`Unknown tool(s): ${bad.join(', ')}`)
      try {
        return ok(json(await bridge('macro:create', { name, description, steps, vars })))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'update_macro',
    {
      description:
        'Overwrite fields of a saved macro. Passing `steps` replaces the whole list — call get_macro first, then send the edited sequence. To drop steps by position, prefer remove_macro_steps.',
      inputSchema: {
        id: z.string().describe('Macro id from list_macros'),
        name: z.string().optional(),
        description: z.string().optional(),
        steps: z.array(stepSchema).optional().describe('Replaces every existing step'),
        vars: z.string().optional().describe('JSON object (as text) for {{var}} placeholders')
      }
    },
    async ({ id, name, description, steps, vars }) => {
      if (steps) {
        const bad = await unknownTools(steps)
        if (bad.length) return err(`Unknown tool(s): ${bad.join(', ')}`)
      }
      try {
        return ok(json(await bridge('macro:update', { id, name, description, steps, vars })))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'remove_macro_steps',
    {
      description:
        'Delete steps from a saved macro by their zero-based position, keeping the rest. Use for "drop this action from the macro". Returns the macro as it now stands.',
      inputSchema: {
        id: z.string().describe('Macro id from list_macros'),
        indexes: z
          .array(z.number().int().nonnegative())
          .describe('Zero-based step positions to delete, as shown by get_macro')
      }
    },
    async ({ id, indexes }) => {
      try {
        return ok(json(await bridge('macro:remove-steps', { id, indexes })))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'run_macro',
    {
      description:
        'Replay a saved macro on the active tab: its tool calls run in order and stop at the first failure. Returns each step\'s status and output. Use when the user asks to run a macro by name — call list_macros first to find its id.',
      inputSchema: {
        id: z.string().describe('Macro id from list_macros'),
        vars: z
          .string()
          .optional()
          .describe("JSON object (as text) overriding the macro's saved {{var}} values for this run")
      }
    },
    async ({ id, vars }) => {
      let resolved: { name: string; steps: RunStep[] }
      try {
        resolved = (await bridge('macro:resolve', { id, vars })) as {
          name: string
          steps: RunStep[]
        }
      } catch (e) {
        return err(errorMessage(e))
      }
      const results = await runWorkflow(resolved.steps, () => {})
      const failed = results.find((r) => r.status === 'error')
      const body = json({ macro: resolved.name, steps: results })
      return failed ? err(body) : ok(body)
    }
  )

  mcp.registerTool(
    'delete_macro',
    {
      description: 'Delete a saved macro entirely.',
      inputSchema: { id: z.string().describe('Macro id from list_macros') }
    },
    async ({ id }) => {
      try {
        return ok(json(await bridge('macro:delete', { id })))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
