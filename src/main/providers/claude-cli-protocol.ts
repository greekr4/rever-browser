import type {
  RequestPermissionRequest,
  RequestPermissionResponse,
  SessionNotification
} from '@agentclientprotocol/sdk'

// Claude Code headless protocol (`claude -p --input-format stream-json
// --output-format stream-json`), translated to the ACP SessionNotification
// shape the renderer already consumes. Pure functions only — the process
// plumbing lives in claude-cli-provider.ts. Shapes verified against
// Claude Code 2.1.318 on 2026-11-06.

export const MCP_SERVER_NAME = 'rever-traffic'

/** Tools that are plumbing, not user-visible work (deferred MCP schema loading). */
const HIDDEN_TOOLS = new Set(['ToolSearch'])

export interface CliModel {
  value: string
  displayName?: string
  description?: string
  resolvedModel?: string
}

export function claudeCliArgs(opts: { mcpUrl: string; authHeader: string; model?: string | null }): string[] {
  const mcp = {
    mcpServers: {
      [MCP_SERVER_NAME]: {
        type: 'http',
        url: opts.mcpUrl,
        headers: { Authorization: opts.authHeader, 'X-Rever-Catalog': 'follow' }
      }
    }
  }
  return [
    '-p',
    '--input-format', 'stream-json',
    '--output-format', 'stream-json',
    '--verbose',
    '--include-partial-messages',
    '--mcp-config', JSON.stringify(mcp),
    '--strict-mcp-config',
    // Same isolation the ACP path used (settingSources: []): no user hooks or settings.
    // Not --bare: that skips the keychain, which is where the subscription login lives.
    '--setting-sources', '',
    '--permission-mode', 'manual',
    // Route every permission prompt to us as a can_use_tool control_request.
    '--permission-prompt-tool', 'stdio',
    ...(opts.model ? ['--model', opts.model] : [])
  ]
}

export const line = (o: unknown): string => JSON.stringify(o) + '\n'

export function userMessage(text: string): string {
  return line({ type: 'user', message: { role: 'user', content: [{ type: 'text', text }] } })
}

export function controlRequest(requestId: string, request: Record<string, unknown>): string {
  return line({ type: 'control_request', request_id: requestId, request })
}

export interface TranslateState {
  hidden: Set<string>
}

export const newTranslateState = (): TranslateState => ({ hidden: new Set() })

// CLI messages are loosely typed JSON; fields are checked where read.
type Msg = Record<string, any>

function note(sessionId: string, update: Record<string, unknown>): SessionNotification {
  return { sessionId, update } as unknown as SessionNotification
}

function resultText(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''
  return content
    .map((c: Msg) => (c?.type === 'text' ? c.text : c?.type === 'image' ? '[image]' : ''))
    .filter(Boolean)
    .join('\n')
}

/** One CLI stdout message → zero or more ACP updates. */
export function translate(msg: Msg, sessionId: string, state: TranslateState): SessionNotification[] {
  if (msg.type === 'stream_event') {
    const ev = msg.event
    if (ev?.type === 'content_block_delta') {
      const d = ev.delta
      if (d?.type === 'text_delta' && d.text) {
        return [note(sessionId, { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: d.text } })]
      }
      if (d?.type === 'thinking_delta' && d.thinking) {
        return [note(sessionId, { sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text: d.thinking } })]
      }
    }
    if (ev?.type === 'content_block_start' && ev.content_block?.type === 'tool_use') {
      const b = ev.content_block
      if (HIDDEN_TOOLS.has(b.name)) {
        state.hidden.add(b.id)
        return []
      }
      return [note(sessionId, { sessionUpdate: 'tool_call', toolCallId: b.id, title: b.name, status: 'pending' })]
    }
    return []
  }

  // Complete assistant messages repeat the streamed text; only the final tool input is new.
  if (msg.type === 'assistant') {
    return (msg.message?.content ?? [])
      .filter((c: Msg) => c.type === 'tool_use' && !state.hidden.has(c.id))
      .map((c: Msg) =>
        note(sessionId, { sessionUpdate: 'tool_call_update', toolCallId: c.id, rawInput: c.input, status: 'in_progress' })
      )
  }

  if (msg.type === 'user') {
    const content = msg.message?.content
    if (!Array.isArray(content)) return []
    return content
      .filter((c: Msg) => c.type === 'tool_result' && !state.hidden.has(c.tool_use_id))
      .map((c: Msg) => {
        const text = resultText(c.content)
        return note(sessionId, {
          sessionUpdate: 'tool_call_update',
          toolCallId: c.tool_use_id,
          status: c.is_error ? 'failed' : 'completed',
          rawOutput: text,
          content: [{ type: 'content', content: { type: 'text', text } }]
        })
      })
  }

  return []
}

/** A `result` message → the turn's stop reason, or an error to throw. */
export function turnOutcome(msg: Msg, interrupted: boolean): { stopReason: string } | { error: string } {
  if (interrupted) return { stopReason: 'cancelled' }
  if (msg.subtype === 'success') return { stopReason: msg.stop_reason ?? 'end_turn' }
  const detail = Array.isArray(msg.errors) && msg.errors.length ? msg.errors.join('; ') : msg.subtype
  return { error: `Claude Code turn failed: ${detail}` }
}

const PERMISSION_OPTIONS = [
  { optionId: 'allow', name: 'Allow', kind: 'allow_once' },
  { optionId: 'allow_always', name: 'Always allow', kind: 'allow_always' },
  { optionId: 'reject', name: 'Reject', kind: 'reject_once' }
]

/** can_use_tool control_request → the ACP permission request the renderer's queue understands. */
export function toPermissionRequest(sessionId: string, req: Msg): RequestPermissionRequest {
  return {
    sessionId,
    // title carries the tool id, which risky-tool matching looks for.
    toolCall: { toolCallId: req.tool_use_id ?? req.tool_name, title: req.tool_name, rawInput: req.input },
    options: PERMISSION_OPTIONS
  } as unknown as RequestPermissionRequest
}

/** ACP permission answer → the control_response the CLI expects. */
export function permissionResponse(requestId: string, answer: RequestPermissionResponse, input: unknown): string {
  const outcome = answer.outcome as { outcome: string; optionId?: string }
  const allow = outcome.outcome === 'selected' && (outcome.optionId ?? '').startsWith('allow')
  const response = allow
    ? { behavior: 'allow', updatedInput: input ?? {} }
    : { behavior: 'deny', message: 'The user rejected this tool call.' }
  return line({ type: 'control_response', response: { subtype: 'success', request_id: requestId, response } })
}

export function errorResponse(requestId: string, error: string): string {
  return line({ type: 'control_response', response: { subtype: 'error', request_id: requestId, error } })
}

/** initialize's model list → the renderer's model-picker state. */
export function modelState(models: CliModel[], current: string | null) {
  return {
    availableModels: models.map((m) => ({ modelId: m.value, name: m.displayName ?? m.value, description: m.description ?? null })),
    currentModelId: current
  }
}
