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

// Where a request field may live in a tool's input: top level, or inside the
// wrapper object each tool's schema uses (replay_request → overrides,
// repeater_send → modifications).
const INPUT_WRAPPERS = ['request', 'overrides', 'modifications'] as const

function inputField(rawInput: unknown, key: string): unknown {
  if (!rawInput || typeof rawInput !== 'object') return undefined
  const o = rawInput as Record<string, unknown>
  if (o[key] !== undefined) return o[key]
  for (const w of INPUT_WRAPPERS) {
    const inner = o[w]
    if (inner && typeof inner === 'object') {
      const v = (inner as Record<string, unknown>)[key]
      if (v !== undefined) return v
    }
  }
  return undefined
}

export function extractMethod(rawInput: unknown): string | null {
  const m = inputField(rawInput, 'method')
  return typeof m === 'string' ? m : null
}

export function extractTargetHost(rawInput: unknown): string | null {
  const url = inputField(rawInput, 'url')
  if (typeof url === 'string') {
    try {
      return new URL(url).host
    } catch {
      return null
    }
  }
  const host = inputField(rawInput, 'host')
  if (typeof host === 'string') return host
  const cmdUrl = shellCommandOf(rawInput)?.match(/https?:\/\/[^\s'"`)]+/)?.[0]
  if (cmdUrl) {
    try {
      return new URL(cmdUrl).host
    } catch {
      return null
    }
  }
  return null
}

// Shell commands that send HTTP themselves (Bash → curl / wget / inline
// Python or Node). They skip the rever tools' scope check, approval prompt and
// audit trail, so any such command is gated under this pseudo tool id.
export const SHELL_HTTP_TOOL = 'shell_http'

// An HTTP client in command position: line start or after ; & | ( ` $( —
// so `grep curl src/` (curl as an argument) does not match.
const SHELL_HTTP_CLIENT_RE =
  /(?:^|[;&|(`]|\$\()\s*(?:sudo\s+|env\s+)?(?:curl|wget|xh|https?|ncat|nc)(?=\s|$)/m
const INLINE_SCRIPT_HTTP_RE =
  /(?:^|[;&|(`]|\$\()\s*(?:python3?|node|bun|deno)\b[^;&|]*\b(?:requests|urllib|httpx|aiohttp|fetch|axios|https?\.request)\b/m

export function shellCommandOf(rawInput: unknown): string | null {
  if (!rawInput || typeof rawInput !== 'object') return null
  const c = (rawInput as Record<string, unknown>).command
  return typeof c === 'string' ? c : null
}

export function isShellHttpCommand(command: string): boolean {
  return SHELL_HTTP_CLIENT_RE.test(command) || INLINE_SCRIPT_HTTP_RE.test(command)
}

function isShellHttpRequest(req: RequestPermissionRequest): boolean {
  const cmd = shellCommandOf(toolCallOf(req)?.rawInput)
  return cmd !== null && isShellHttpCommand(cmd)
}

// Returns the matched risky tool id, or null when the request may be
// auto-approved. Method-gated tools return their id only for non-idempotent
// methods; an unknown method is treated as risky (fail safe).
export function matchedRiskyTool(req: RequestPermissionRequest): string | null {
  if (isShellHttpRequest(req)) return SHELL_HTTP_TOOL
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

// Gated = any tool on either list, whatever the method. These must never be
// answered with allow_always: claude-agent-acp turns that into a session rule
// for the tool name, after which it stops asking — so one approval (or one
// auto-approved GET replay) would silently disable the prompt for every later
// call, POSTs included.
export function gatedToolName(req: RequestPermissionRequest): string | null {
  if (isShellHttpRequest(req)) return SHELL_HTTP_TOOL
  const hay = toolHaystack(req)
  return [...ALWAYS_RISKY_TOOLS, ...METHOD_GATED_TOOLS].find((t) => hay.includes(t)) ?? null
}

export function isGatedToolRequest(req: RequestPermissionRequest): boolean {
  return gatedToolName(req) !== null
}

function optionOfKind(req: RequestPermissionRequest, kind: string): string | undefined {
  return req.options.find((o) => o.kind === kind)?.optionId
}

export function findRejectOption(req: RequestPermissionRequest): string {
  const reject = req.options.find((o) => o.kind.startsWith('reject'))
  return reject?.optionId ?? req.options.at(0)?.optionId ?? ''
}

// The allow option to use when approving: allow_once for gated tools,
// otherwise allow_always (so ordinary tools don't re-prompt every call).
export function chooseAllowOption(req: RequestPermissionRequest): string {
  const preferred = isGatedToolRequest(req)
    ? optionOfKind(req, 'allow_once')
    : optionOfKind(req, 'allow_always')
  if (preferred) return preferred
  const allow = req.options.find((o) => o.kind.startsWith('allow'))
  return allow?.optionId ?? req.options.at(0)?.optionId ?? ''
}

// Downgrades an explicit "Always Allow" pick on a gated tool to allow_once.
export function sanitizeSelection(req: RequestPermissionRequest, optionId: string): string {
  if (!isGatedToolRequest(req)) return optionId
  const picked = req.options.find((o) => o.optionId === optionId)
  if (picked?.kind !== 'allow_always') return optionId
  return optionOfKind(req, 'allow_once') ?? optionId
}

// Main-process answer when the renderer prompt can't be reached: never let a
// risky action through unseen.
export function fallbackOption(req: RequestPermissionRequest): string {
  return isRiskyToolRequest(req) ? findRejectOption(req) : chooseAllowOption(req)
}
