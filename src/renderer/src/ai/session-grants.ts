import { gatedToolName } from './risky-tools'

import type { RequestPermissionRequest } from '@agentclientprotocol/sdk'

// "Allow all <tool> calls this session" grants for gated tools. Kept here in
// the renderer instead of answering allow_always: that would hand the decision
// to claude-agent-acp, which then stops asking — and stops the audit trail.
// Keyed by ACP session, so a new chat starts with no grants. In-memory only:
// an app restart clears them too.
const grants = new Set<string>()

function grantKey(req: RequestPermissionRequest): string | null {
  const tool = gatedToolName(req)
  return tool ? `${req.sessionId}:${tool}` : null
}

export function grantSession(req: RequestPermissionRequest): void {
  const key = grantKey(req)
  if (key) grants.add(key)
}

export function hasSessionGrant(req: RequestPermissionRequest): boolean {
  const key = grantKey(req)
  return key !== null && grants.has(key)
}

export function clearSessionGrants(): void {
  grants.clear()
}
