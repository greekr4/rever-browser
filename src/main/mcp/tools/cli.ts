import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import {
  cliRunExample,
  describeCli,
  exportCurl,
  parseExtract,
  type CliData,
  type CliExtract
} from '../../../shared/cli-spec'
import { askRenderer } from '../../renderer-bridge'
import { armCli, compileCaptured, executeCli } from '../cli-exec'
import { err, errorMessage, ok } from '../utils'

function json(value: unknown): string {
  return JSON.stringify(value, null, 2)
}

const RAW_HTML_NOTE =
  'Response was cut off before the end of the page. Pass extract once — selector, fields ({ name, selector?, attr? }), optional limit — and later runs return those fields instead of the page.'

const extractSchema = z
  .object({
    selector: z.string().describe('CSS selector that matches each row'),
    limit: z.number().int().min(1).max(20).optional().describe('Rows to return. Default 5, maximum 20'),
    fields: z
      .array(
        z.object({
          name: z.string().describe('Field name, such as title or link'),
          selector: z.string().optional().describe('CSS selector relative to the row. Omit to use the row itself'),
          attr: z.string().optional().describe('Attribute to read, such as href. Omit to read the text')
        })
      )
      .min(1)
      .max(6)
  })
  .describe('Fields to read from an HTML response. Saved on the command.')

function takeExtract(raw: unknown): { ok: true; extract: CliExtract } | { ok: false; error: string } {
  const parsed = parseExtract(raw)
  if (!parsed.ok) return parsed
  if (!parsed.extract) return { ok: false, error: 'extract must be an object' }
  return { ok: true, extract: parsed.extract }
}

interface SavedCli {
  id: string
  name: string
  site: string
  baseUrl: string
  writes: boolean
  commands: Array<{ name: string; method: string; path: string; effect: string }>
}

export function registerCliTools(mcp: McpServer): void {
  mcp.registerTool(
    'cli_arm',
    {
      description:
        'Start, stop, or list a recording window of XHR and Fetch calls on the active tab. The list is request ids only and does not save a workflow. Static files are left out. Pass the ids you mean to keep to cli_compile.',
      inputSchema: {
        action: z.enum(['start', 'stop', 'list']).describe('start begins a window, stop ends it, list returns the calls inside it')
      }
    },
    async ({ action }) => ok(json(armCli(action)))
  )

  mcp.registerTool(
    'cli_compile',
    {
      description:
        'Turn explicit captured request ids into one site CLI in the Workflows panel. Only those ids are included. Writes stay off. Pass extract to teach every new command which HTML fields to return. Returns the workflow id and command names, never header or body values.',
      inputSchema: {
        requestIds: z.array(z.string()).min(1).describe('requestId values from list_requests or cli_arm'),
        name: z.string().optional().describe('Name shown in the Workflows panel'),
        site: z.string().optional().describe('Site slug. Defaults to the host'),
        title: z.string().optional(),
        baseUrl: z.string().optional().describe('Origin to keep. Other origins are dropped'),
        names: z
          .record(z.string(), z.string())
          .optional()
          .describe('Command name per requestId, named for what it does, e.g. { "123.4": "search" }. Omit for names derived from the path'),
        extract: extractSchema.optional()
      }
    },
    async ({ requestIds, name, site, title, baseUrl, names, extract }) => {
      try {
        const data = compileCaptured(requestIds, { site, title, baseUrl, names })
        if (data.commands.length === 0) return err('no commands from those request ids')
        if (extract) {
          const parsed = takeExtract(extract)
          if (!parsed.ok) return err(parsed.error)
          data.commands = data.commands.map((command) => ({ ...command, extract: parsed.extract }))
        }
        const saved = (await askRenderer('cli:save', {
          name: name?.trim() || (data.commands.length === 1 ? data.commands[0].name : data.site),
          data
        })) as SavedCli
        return ok(json(saved))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'cli_list',
    {
      description:
        'List saved site CLIs: id, site, command names, effects, and whether writes are on. No header values or secrets.',
      inputSchema: {}
    },
    async () => {
      try {
        return ok(json(await askRenderer('cli:list', {})))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'cli_describe',
    {
      description:
        'Describe one saved CLI, or one of its commands: method, path, effect, flags, session header names, saved extract, and evidence request ids. The reply ends with the cli_run object (id, command, inputs). Inputs are blank here. The Workflows panel fills them from the captured request. Cookie values are never included.',
      inputSchema: {
        id: z.string().describe('CLI id from cli_list or cli_compile'),
        command: z.string().optional().describe('One command name. Omit to describe every command')
      }
    },
    async ({ id, command }) => {
      try {
        const row = (await askRenderer('cli:get', { id })) as { name: string; data: CliData }
        const described = describeCli(row.data, command)
        const commands = command
          ? row.data.commands.filter((item) => item.name === command)
          : row.data.commands
        const calls = commands.map((item) => cliRunExample(id, item, {})).join('\n')
        return ok(calls ? `${described}\ncli_run:\n${calls}\n` : described)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'cli_run',
    {
      description:
        'Run one saved CLI command inside the logged-in tab. Cookies stay in the tab. Read commands send immediately. Write and unknown commands send only when writes are on for that CLI. A failure after a write was sent returns action_outcome_unknown and is not retried. Pass extract once to teach the command which fields to read from an HTML page. Later runs use the saved extract and return those fields instead of the page. Without extract, an HTML page comes back as a short preview.',
      inputSchema: {
        id: z.string().describe('CLI id from cli_list or cli_compile'),
        command: z.string().describe('Command name from cli_describe'),
        inputs: z
          .record(z.string(), z.string())
          .optional()
          .describe('Flag values keyed by flag name. Required path flags must be present'),
        extract: extractSchema.optional()
      }
    },
    async ({ id, command, inputs, extract }) => {
      try {
        const row = (await askRenderer('cli:get', { id })) as { data: CliData }
        let found = row.data.commands.find((item) => item.name === command)
        if (!found) return err(`unknown command: ${command}`)
        if (extract) {
          const parsed = takeExtract(extract)
          if (!parsed.ok) return err(parsed.error)
          await askRenderer('cli:set-extract', { id, command, extract: parsed.extract })
          found = { ...found, extract: parsed.extract }
        }
        const result = await executeCli({
          baseUrl: row.data.baseUrl,
          writes: row.data.writes,
          command: found,
          inputs: inputs ?? {}
        })
        if (!result.ok) return err(result.error ?? 'cli run failed')
        if (result.items) {
          return ok(
            json({
              status: result.status,
              matched: result.matched ?? result.items.length,
              items: result.items,
              ...(result.note ? { note: result.note } : {})
            })
          )
        }
        return ok(
          json({
            status: result.status,
            body: result.body ?? '',
            ...(result.bodyCut ? { note: RAW_HTML_NOTE } : {})
          })
        )
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'cli_writes',
    {
      description:
        'Turn writes on or off for one saved CLI. Ask the user once before turning writes on. This only flips the saved flag. It does not send a request.',
      inputSchema: {
        id: z.string().describe('CLI id from cli_list or cli_compile'),
        enabled: z.boolean().describe('true allows write and unknown commands to send')
      }
    },
    async ({ id, enabled }) => {
      try {
        return ok(json(await askRenderer('cli:set-writes', { id, writes: enabled })))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'cli_export',
    {
      description:
        'Return a curl script for a saved CLI. Flags are placeholders. Captured secrets and cookie values are not included.',
      inputSchema: {
        id: z.string().describe('CLI id from cli_list or cli_compile')
      }
    },
    async ({ id }) => {
      try {
        const row = (await askRenderer('cli:get', { id })) as { data: CliData }
        return ok(exportCurl(row.data))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
