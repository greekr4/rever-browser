import { createServer, type IncomingMessage, type ServerResponse } from 'node:http'
import { randomUUID } from 'node:crypto'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'
import { StreamableHTTPServerTransport } from '@modelcontextprotocol/sdk/server/streamableHttp.js'
import { isInitializeRequest } from '@modelcontextprotocol/sdk/types.js'

import { registerBrowserTools } from './tools/browser'
import { registerScriptTools } from './tools/scripts'
import { registerTrafficTools } from './tools/traffic'
import { registerAuthTools } from './tools/auth'
import { registerDecodeTools } from './tools/decode'
import { registerDiffTools } from './tools/diff'
import { registerSourceMapTools } from './tools/sourcemap'
import { registerConsoleTools } from './tools/console'
import { registerInjectTools } from './tools/inject'
import { registerWebSocketTools } from './tools/websocket'
import { registerInterceptTools } from './tools/intercept'
import { registerDebuggerTools } from './tools/debugger'

function buildMcpServer(): McpServer {
  const mcp = new McpServer({ name: 'rever-traffic', version: '0.1.0' })
  registerTrafficTools(mcp)
  registerBrowserTools(mcp)
  registerScriptTools(mcp)
  registerAuthTools(mcp)
  registerDecodeTools(mcp)
  registerDiffTools(mcp)
  registerSourceMapTools(mcp)
  registerConsoleTools(mcp)
  registerInjectTools(mcp)
  registerWebSocketTools(mcp)
  registerInterceptTools(mcp)
  registerDebuggerTools(mcp)
  return mcp
}

interface RunningServer {
  url: string
  close: () => Promise<void>
}

async function readBody(req: IncomingMessage): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = []
    req.on('data', (c: Buffer) => chunks.push(c))
    req.on('end', () => {
      const raw = Buffer.concat(chunks).toString('utf8')
      if (!raw) {
        resolve(undefined)
        return
      }
      try {
        resolve(JSON.parse(raw))
      } catch (e) {
        reject(e)
      }
    })
    req.on('error', reject)
  })
}

let cached: Promise<RunningServer> | null = null

export function startMcpServer(): Promise<RunningServer> {
  if (cached) return cached
  cached = (async () => {
    const transports = new Map<string, StreamableHTTPServerTransport>()

    const handle = async (req: IncomingMessage, res: ServerResponse) => {
      try {
        if (req.url !== '/mcp') {
          res.statusCode = 404
          res.end('not found')
          return
        }

        const sessionId = req.headers['mcp-session-id'] as string | undefined
        let transport = sessionId ? transports.get(sessionId) : undefined

        if (req.method === 'POST') {
          const body = await readBody(req)

          if (!transport) {
            if (!isInitializeRequest(body)) {
              res.statusCode = 400
              res.end('missing or invalid session')
              return
            }
            transport = new StreamableHTTPServerTransport({
              sessionIdGenerator: () => randomUUID(),
              onsessioninitialized: (sid) => {
                transports.set(sid, transport!)
              }
            })
            transport.onclose = () => {
              if (transport!.sessionId) transports.delete(transport!.sessionId)
            }
            const mcp = buildMcpServer()
            await mcp.connect(transport)
          }

          await transport.handleRequest(req, res, body)
          return
        }

        if (req.method === 'GET' || req.method === 'DELETE') {
          if (!transport) {
            res.statusCode = 400
            res.end('no session')
            return
          }
          await transport.handleRequest(req, res)
          return
        }

        res.statusCode = 405
        res.end('method not allowed')
      } catch (e) {
        console.error('[mcp] handler error:', e)
        if (!res.headersSent) {
          res.statusCode = 500
          res.end('internal error')
        }
      }
    }

    const server = createServer((req, res) => {
      void handle(req, res)
    })

    await new Promise<void>((resolve) => {
      server.listen(0, '127.0.0.1', () => resolve())
    })
    const addr = server.address()
    if (!addr || typeof addr === 'string') throw new Error('failed to bind MCP server')
    const url = `http://127.0.0.1:${addr.port}/mcp`
    console.log('[mcp] listening on', url)

    return {
      url,
      async close() {
        await new Promise<void>((resolve) => server.close(() => resolve()))
      }
    }
  })()
  return cached
}
