import { z } from 'zod'
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { askRenderer } from '../../renderer-bridge'
import { ok, err } from '../utils'

/**
 * Launch the deterministic Red Team pipeline (the one behind the Red Team
 * popup): discovery → parallel per-tab worker agents → optional probe phase →
 * merged report. This is the ONLY way one agent command covers dozens of tabs
 * — hand-orchestrating sub-agents per tab collides on the shared browser, so
 * steer here instead of spawning Task sub-agents for site-wide sweeps.
 */
export function registerScanTools(mcp: McpServer) {
  mcp.registerTool(
    'redteam_start',
    {
      description:
        'Launch a site-wide Red Team scan of one target URL: opens up to N same-site pages as background tabs, runs one inspecting agent per tab in parallel (read + dynamic exploration), then (at probe/redteam intensity) single-shot probes against the deduped endpoint set, and finishes with a merged Markdown report. The user confirms the run in the Red Team popup — active intensities are never started silently. Use this instead of spawning sub-agents per tab. Poll progress with finding_list / list_requests; the final report is exported by finding_export.',
      inputSchema: {
        url: z.string().describe('Absolute start URL of the site to scan (scheme required)'),
        pages: z
          .number()
          .int()
          .optional()
          .describe('Page budget, 1-200 (default 20)'),
        intensity: z
          .enum(['readonly', 'probe', 'redteam'])
          .optional()
          .describe(
            'readonly = observe only; probe = replay captured requests unchanged + security headers; redteam = additionally one mutated/payload request per endpoint (default readonly)'
          )
      }
    },
    async ({ url, pages, intensity }) => {
      if (!/^https?:\/\//i.test(url)) return err('url must be absolute, including the scheme')
      try {
        const res = (await askRenderer('scan:launch', {
          url,
          pages: pages ?? 20,
          intensity: intensity ?? 'readonly'
        }, 8000)) as { phase?: string } | undefined
        return ok(
          `scan armed (${intensity ?? 'readonly'}) — the user confirms it in the Red Team popup; nothing runs until then. ` +
            `Once running, findings appear via finding_add (poll finding_list) and the merged report is produced by finding_export at the end. Reply state: ${res?.phase ?? 'confirm'}`
        )
      } catch (e) {
        return err(`could not arm the scan: ${e instanceof Error ? e.message : String(e)}`)
      }
    }
  )
}
