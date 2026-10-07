import type { RequestPermissionRequest } from '@agentclientprotocol/sdk'

// Tools whose side effects are destructive or high-volume enough that they must
// always prompt the user, regardless of the global auto-approve setting.
// These names mirror the MCP tool ids registered in src/main/mcp/tools/*.
export const ALWAYS_RISKY_TOOLS = [
  'intruder_run',
  'burst_send',
  'payload_probe',
  'crlf_test',
  'path_probe',
  'lfi_probe'
] as const

// Replay-style tools are risky only for non-idempotent methods. A GET/HEAD
// replay is safe to auto-approve; a POST/PUT/DELETE replay on the user's live
// authenticated session is not.
export const METHOD_GATED_TOOLS = ['replay_request', 'repeater_send'] as const

const SAFE_METHODS = new Set(['GET', 'HEAD'])

type ToolCallLike = { toolCallId?: string; title?: string; rawInput?: unknown } | undefined

function toolCallOf(req: RequestPermissionRequest): ToolCallLike {
  return (req as unknown as { toolCall?: ToolCallLike }).toolCall
}

// claude-agent-acp exposes no dedicated tool-name field on the permission
// request, so we match against the toolCallId + human title (which for MCP
// tools carry the tool id, e.g. `mcp__rever-traffic__intruder_run`).
export function toolHaystack(req: RequestPermissionRequest): string {
  const tc = toolCallOf(req)
  return `${tc?.toolCallId ?? ''} ${tc?.title ?? ''}`.toLowerCase()
}

export function extractMethod(rawInput: unknown): string | null {
  if (rawInput && typeof rawInput === 'object') {
    const o = rawInput as Record<string, unknown>
    const nested =
      o.request && typeof o.request === 'object'
        ? (o.request as Record<string, unknown>).method
        : undefined
    const m = o.method ?? nested
    if (typeof m === 'string') return m
  }
  return null
}

export function extractTargetHost(rawInput: unknown): string | null {
  if (rawInput && typeof rawInput === 'object') {
    const o = rawInput as Record<string, unknown>
    const nestedUrl =
      o.request && typeof o.request === 'object'
        ? (o.request as Record<string, unknown>).url
        : undefined
    const url = o.url ?? nestedUrl
    if (typeof url === 'string') {
      try {
        return new URL(url).host
      } catch {
        return null
      }
    }
    if (typeof o.host === 'string') return o.host
  }
  return null
}

// Returns the matched risky tool id, or null when the request may be
// auto-approved. Method-gated tools return their id only for non-idempotent
// methods; an unknown method is treated as risky (fail safe).
export function matchedRiskyTool(req: RequestPermissionRequest): string | null {
  const hay = toolHaystack(req)
  for (const t of ALWAYS_RISKY_TOOLS) if (hay.includes(t)) return t
  for (const t of METHOD_GATED_TOOLS) {
    if (!hay.includes(t)) continue
    const method = extractMethod(toolCallOf(req)?.rawInput)
    if (!method || !SAFE_METHODS.has(method.toUpperCase())) return t
  }
  return null
}

export function isRiskyToolRequest(req: RequestPermissionRequest): boolean {
  return matchedRiskyTool(req) !== null
}
