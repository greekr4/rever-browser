import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { getAskWhereToSave, listDownloads, waitForDownload } from '../../downloads'
import { ok } from '../utils'

export function registerDownloadTools(mcp: McpServer) {
  mcp.registerTool(
    'list_downloads',
    {
      description:
        'List files the page downloaded (newest last): filename, url, mime, bytes, state (progressing/completed/cancelled/interrupted) and savePath. Downloads are saved to ~/Downloads/Rever without a save dialog unless the user turned on "ask where to save".',
      inputSchema: { limit: z.number().int().positive().max(100).optional().describe('Default 20') }
    },
    async ({ limit }) =>
      ok(JSON.stringify({ askWhereToSave: getAskWhereToSave(), downloads: listDownloads(limit ?? 20) }, null, 2))
  )

  mcp.registerTool(
    'wait_for_download',
    {
      description:
        'Wait until a download finishes and return it (savePath, bytes, state). Call right after the click that starts a download. Returns a download that finished in the last 30s if you have not seen it yet, so calling it after a fast download is fine. Each download is returned once.',
      inputSchema: {
        timeoutMs: z.number().int().positive().max(120000).optional().describe('Default 30000')
      }
    },
    async ({ timeoutMs }) => {
      const d = await waitForDownload(timeoutMs ?? 30000)
      if (!d) return ok('no download finished before the timeout')
      return ok(JSON.stringify(d, null, 2))
    }
  )
}
