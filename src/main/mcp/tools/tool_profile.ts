import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { err, ok } from '../utils'
import {
  TOOL_PROFILE_SET_DESCRIPTION,
  advertisedNames,
  toolProfileReadText,
  toolProfileSwitchedText
} from '../tool-profile'
import { getToolProfile, setToolProfile } from '../tool-profile-state'

export function registerToolProfileTools(mcp: McpServer): void {
  mcp.registerTool(
    'tool_profile_set',
    {
      description: TOOL_PROFILE_SET_DESCRIPTION,
      inputSchema: {
        profile: z
          .enum(['browse', 'reverse', 'probe'])
          .optional()
          .describe('Omit to read. browse or reverse switches. probe is rejected for the agent.')
      }
    },
    async ({ profile }) => {
      if (profile == null) {
        const current = getToolProfile()
        return ok(toolProfileReadText(current, advertisedNames(current).length))
      }
      const result = setToolProfile(profile, 'agent')
      if (!result.ok) return err(result.error)
      return ok(toolProfileSwitchedText(result.profile, result.advertised))
    }
  )
}
