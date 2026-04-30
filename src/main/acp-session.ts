import { spawn, type ChildProcessByStdio } from 'node:child_process'
import { Readable, Writable } from 'node:stream'
import {
  ClientSideConnection,
  ndJsonStream,
  PROTOCOL_VERSION,
  type Agent,
  type Client,
  type RequestPermissionRequest,
  type RequestPermissionResponse,
  type SessionNotification
} from '@agentclientprotocol/sdk'
import type { WebContents } from 'electron'

import { startMcpServer } from './mcp/server'

export interface AgentDef {
  id: string
  command: string
  args: string[]
}

interface SessionEntry {
  agentDef: AgentDef
  child: ChildProcessByStdio<Writable, Readable, Readable>
  connection: ClientSideConnection
  sessionId: string
  onUpdate: ((n: SessionNotification) => void) | null
  dead: boolean
}

const sessions = new Map<string, SessionEntry>()

function pickAutoApproveOption(req: RequestPermissionRequest): string {
  const allowAlways = req.options.find((o) => o.kind === 'allow_always')
  if (allowAlways) return allowAlways.optionId
  const allow = req.options.find((o) => o.kind.startsWith('allow'))
  return allow?.optionId ?? req.options[0]?.optionId ?? ''
}

export async function spawnAcpSession(
  agentDef: AgentDef,
  cwd: string
): Promise<{ sessionId: string }> {
  const child = spawn(agentDef.command, agentDef.args, {
    cwd,
    stdio: ['pipe', 'pipe', 'pipe']
  }) as ChildProcessByStdio<Writable, Readable, Readable>

  child.stderr.on('data', (buf: Buffer) => {
    console.error(`[ACP ${agentDef.id}]`, buf.toString())
  })

  const input = Writable.toWeb(child.stdin) as WritableStream<Uint8Array>
  const output = Readable.toWeb(child.stdout) as ReadableStream<Uint8Array>
  const stream = ndJsonStream(input, output)

  let entryRef: SessionEntry | null = null

  const clientImpl: Client = {
    async requestPermission(
      params: RequestPermissionRequest
    ): Promise<RequestPermissionResponse> {
      // M0: auto-approve. UI permission round-trip wired in M0.5+.
      return {
        outcome: { outcome: 'selected', optionId: pickAutoApproveOption(params) }
      }
    },
    async sessionUpdate(params: SessionNotification): Promise<void> {
      entryRef?.onUpdate?.(params)
    }
  }

  const connection = new ClientSideConnection((_agent: Agent) => clientImpl, stream)

  await connection.initialize({
    protocolVersion: PROTOCOL_VERSION,
    clientCapabilities: {}
  })

  const mcp = await startMcpServer()
  const result = await connection.newSession({
    cwd,
    mcpServers: [
      {
        type: 'http',
        name: 'rever-traffic',
        url: mcp.url,
        headers: []
      }
    ]
  })

  const entry: SessionEntry = {
    agentDef,
    child,
    connection,
    sessionId: result.sessionId,
    onUpdate: null,
    dead: false
  }
  entryRef = entry
  sessions.set(result.sessionId, entry)

  child.on('close', () => {
    entry.dead = true
    sessions.delete(result.sessionId)
  })

  return { sessionId: result.sessionId }
}

export async function promptAcpSession(
  sessionId: string,
  text: string,
  onUpdate: (n: SessionNotification) => void
): Promise<{ stopReason: string }> {
  const entry = sessions.get(sessionId)
  if (!entry) throw new Error(`unknown ACP session: ${sessionId}`)
  if (entry.dead) throw new Error(`ACP session is dead: ${sessionId}`)

  entry.onUpdate = onUpdate
  try {
    const res = await entry.connection.prompt({
      sessionId: entry.sessionId,
      prompt: [{ type: 'text', text }]
    })
    return { stopReason: res.stopReason }
  } finally {
    entry.onUpdate = null
  }
}

export async function cancelAcpSession(sessionId: string): Promise<void> {
  const entry = sessions.get(sessionId)
  if (!entry || entry.dead) return
  await entry.connection.cancel({ sessionId: entry.sessionId }).catch(() => null)
}

export async function killAcpSession(sessionId: string): Promise<void> {
  const entry = sessions.get(sessionId)
  if (!entry) return
  entry.dead = true
  sessions.delete(sessionId)
  entry.child.kill()
}
