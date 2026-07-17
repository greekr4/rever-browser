import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'

import { startMcpServer } from './server'

// A standalone MCP client into the in-process tool server, used by the workflow
// executor to run tools deterministically (no LLM in the loop) and to list the
// available tools for the macro editor. The in-process agent providers keep
// their own client — this one is intentionally separate so workflow features
// never touch the agent hot path.

export interface McpToolInfo {
  name: string
  description?: string
  inputSchema: unknown
}

let connection: Promise<{ client: Client; tools: McpToolInfo[] }> | null = null

function connect(): Promise<{ client: Client; tools: McpToolInfo[] }> {
  return (async () => {
    const { url } = await startMcpServer()
    const client = new Client({ name: 'rever-workflow', version: '0.1.0' })
    await client.connect(new StreamableHTTPClientTransport(new URL(url)))
    const listed = await client.listTools()
    const tools: McpToolInfo[] = listed.tools.map((t) => ({
      name: t.name,
      description: t.description ?? undefined,
      inputSchema: t.inputSchema
    }))
    return { client, tools }
  })()
}

function getConnection(): Promise<{ client: Client; tools: McpToolInfo[] }> {
  if (!connection) connection = connect()
  return connection
}

export async function listMcpTools(): Promise<McpToolInfo[]> {
  const { tools } = await getConnection()
  return tools
}

export async function callMcpTool(
  name: string,
  input: Record<string, unknown>
): Promise<{ text: string; isError: boolean }> {
  const { client } = await getConnection()
  const result = await client.callTool({ name, arguments: input })
  const content = (result as { content?: unknown }).content
  const parts: string[] = []
  if (Array.isArray(content)) {
    for (const c of content) {
      if (c && typeof c === 'object' && (c as { type?: string }).type === 'text') {
        parts.push(String((c as { text?: string }).text ?? ''))
      }
    }
  }
  return {
    text: parts.join('\n'),
    isError: (result as { isError?: boolean }).isError === true
  }
}
