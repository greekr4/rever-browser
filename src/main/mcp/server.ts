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
import { registerRepeaterTools } from './tools/repeater'
import { registerStorageTools } from './tools/storage'
import { registerHeaderTools } from './tools/headers'
import { registerHarTools } from './tools/har'
import { registerIntruderTools } from './tools/intruder'
import { registerOverrideTools } from './tools/override'
import { registerBurstTools } from './tools/burst'
import { registerCryptoTools } from './tools/crypto'
import { registerDeobTools } from './tools/deob'
import { registerPayloadProbeTools } from './tools/payload_probe'
import { registerCrlfTools } from './tools/crlf'
import { registerPathProbeTools } from './tools/path_probe'
import { registerFindingTools } from './tools/findings'
import { registerSecurityInspectTools } from './tools/security_inspect'
import { registerDomEditTools } from './tools/dom_edit'
import { registerServiceWorkerTools } from './tools/sw_inspect'
import { registerDialogTools } from './tools/dialog'
import { registerVisionTools } from './tools/vision'

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
  registerRepeaterTools(mcp)
  // New tools — webhacking.kr-driven feature set
  registerStorageTools(mcp)
  registerHeaderTools(mcp)
  registerHarTools(mcp)
  registerIntruderTools(mcp)
  registerOverrideTools(mcp)
  registerBurstTools(mcp)
  registerCryptoTools(mcp)
  registerDeobTools(mcp)
  registerPayloadProbeTools(mcp)
  registerCrlfTools(mcp)
  registerPathProbeTools(mcp)
  registerFindingTools(mcp)
  registerSecurityInspectTools(mcp)
  registerDomEditTools(mcp)
  registerServiceWorkerTools(mcp)
  registerDialogTools(mcp)
  registerVisionTools(mcp)
  return mcp
}

interface RunningServer {
  url: string
  close: () => Promise<void>
}

const READ_BODY_LIMIT = 16 * 1024 * 1024 // 16MB

async function readBody(req: IncomingMessage): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = []
    let total = 0
    req.on('data', (c: Buffer) => {
      total += c.length
      if (total > READ_BODY_LIMIT) {
        // 413 은 핸들러에서 처리 — 여기서는 에러를 throw해 reject
        reject(Object.assign(new Error('request body too large'), { statusCode: 413 }))
        req.destroy()
        return
      }
      chunks.push(c)
    })
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

    // 바인딩 포트 — listen 후 설정, Origin 검증에 사용
    let boundPort = 0

    // Origin 헤더 DNS rebinding 검증.
    // 에이전트 자체 요청은 Origin 헤더를 보내지 않으므로 origin 없는 경우는 통과.
    function isOriginAllowed(origin: string | undefined): boolean {
      if (!origin) return true // 에이전트 or same-process 요청
      try {
        const { hostname, port } = new URL(origin)
        const p = port || (origin.startsWith('https') ? '443' : '80')
        return (
          (hostname === '127.0.0.1' || hostname === 'localhost') &&
          p === String(boundPort)
        )
      } catch {
        return false
      }
    }

    const handle = async (req: IncomingMessage, res: ServerResponse) => {
      try {
        if (req.url !== '/mcp') {
          res.statusCode = 404
          res.end('not found')
          return
        }

        // POST initialize 시 Origin 헤더 존재하면 신뢰 가능한 출처인지 확인
        if (req.method === 'POST') {
          const origin = req.headers['origin'] as string | undefined
          if (!isOriginAllowed(origin)) {
            res.statusCode = 403
            res.end('forbidden: untrusted origin')
            return
          }
        }

        const sessionId = req.headers['mcp-session-id'] as string | undefined
        let transport = sessionId ? transports.get(sessionId) : undefined

        if (req.method === 'POST') {
          let body: unknown
          try {
            body = await readBody(req)
          } catch (e: unknown) {
            const status = (e as { statusCode?: number }).statusCode === 413 ? 413 : 400
            if (!res.headersSent) {
              res.statusCode = status
              res.end(status === 413 ? 'request body too large' : 'bad request')
            }
            return
          }

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
              },
              enableDnsRebindingProtection: true,
              allowedHosts: [`127.0.0.1:${boundPort}`, `localhost:${boundPort}`]
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
    boundPort = addr.port
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
