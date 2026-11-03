import net from 'node:net'
import tlsMod from 'node:tls'

import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { isLoopbackHost, isLabHost } from '../fence'
import { ok, err, errorMessage } from '../utils'

// Raw HTTP/1.1 sender over a TCP socket. fetch/curl normalise the URL and strip
// the `#` fragment before sending, so they can never put a `#` (or other raw
// bytes) in the request line. Some front proxies parse the request line
// differently from the backend — e.g. HAProxy < 2.8.2 (CVE-2023-45539) stops
// its `path` ACL at `#` while the Node backend routes on the full path, so a
// denied path is reachable as `/x#/denied`. This tool writes the request line
// EXACTLY as given (no encoding, no normalisation) and returns the raw
// response, which is the only way to drive those smuggling/desync bypasses.
// Gated to loopback / explicitly-marked lab hosts, matching the network fence.

export function buildRawRequest(
  method: string,
  path: string,
  headers: Record<string, string>,
  body: string | undefined,
  host: string
): string {
  const hdrLower = new Set(Object.keys(headers).map((k) => k.toLowerCase()))
  const lines = [`${method} ${path} HTTP/1.1`]
  if (!hdrLower.has('host')) lines.push(`Host: ${host}`)
  for (const [k, v] of Object.entries(headers)) lines.push(`${k}: ${v}`)
  if (!hdrLower.has('connection')) lines.push('Connection: close')
  if (body != null && !hdrLower.has('content-length')) {
    lines.push(`Content-Length: ${Buffer.byteLength(body)}`)
  }
  return lines.join('\r\n') + '\r\n\r\n' + (body ?? '')
}

export interface RawResponse {
  statusLine: string
  status: number
  headers: Record<string, string>
  body: string
}

/** Split a raw HTTP response into status line, headers, and body. */
export function parseRawResponse(raw: Buffer): RawResponse {
  const sep = raw.indexOf('\r\n\r\n')
  const headPart = (sep >= 0 ? raw.subarray(0, sep) : raw).toString('latin1')
  const body = sep >= 0 ? raw.subarray(sep + 4).toString('latin1') : ''
  const [statusLine, ...hdrLines] = headPart.split('\r\n')
  const headers: Record<string, string> = {}
  for (const line of hdrLines) {
    const i = line.indexOf(':')
    if (i > 0) headers[line.slice(0, i).trim().toLowerCase()] = line.slice(i + 1).trim()
  }
  const status = Number(statusLine?.split(' ')[1]) || 0
  return { statusLine: statusLine ?? '', status, headers, body }
}

export function registerRawHttpTools(mcp: McpServer) {
  mcp.registerTool(
    'raw_http',
    {
      description:
        'Send a raw HTTP/1.1 request over a TCP socket with the request line written EXACTLY as given — no URL normalisation, no `#`-fragment stripping, raw bytes allowed. Use for front-proxy/backend desync and ACL bypasses that fetch/curl cannot express: HAProxy < 2.8.2 `#`-fragment path-ACL bypass (CVE-2023-45539, reach a denied path as "/x#/denied"), request smuggling, header/method tricks. Either pass a full `raw` request (verbatim bytes) or method+path+headers+body. Gated to loopback / lab hosts (matching the network fence). Returns the parsed status line, headers, and body.',
      inputSchema: {
        host: z.string().describe('Target host, e.g. "127.0.0.1"'),
        port: z.number().int().describe('Target port, e.g. 1337'),
        method: z.string().optional().describe('e.g. GET (ignored if raw is set)'),
        path: z.string().optional().describe('Request target sent verbatim, e.g. "/x#/healthcheck-dev" (ignored if raw is set)'),
        headers: z.record(z.string(), z.string()).optional().describe('Extra request headers'),
        body: z.string().optional(),
        raw: z.string().optional().describe('A complete raw request to send byte-for-byte (use \\r\\n line endings); overrides method/path/headers/body'),
        tls: z.boolean().optional().describe('Wrap in TLS (default false)'),
        timeoutMs: z.number().int().optional().describe('default 8000')
      }
    },
    async ({ host, port, method, path, headers, body, raw, tls, timeoutMs }) => {
      const url = `${tls ? 'https' : 'http'}://${host}:${port}/`
      if (!isLoopbackHost(url) && !isLabHost(url)) {
        return err(
          `raw_http is gated to loopback / lab hosts (fence policy); ${host} is neither. Mark it as a lab target first if you own it.`
        )
      }
      try {
        const payload = raw ?? buildRawRequest(method ?? 'GET', path ?? '/', headers ?? {}, body, `${host}:${port}`)
        const result = await new Promise<Buffer>((resolve, reject) => {
          const chunks: Buffer[] = []
          const onConnect = (): void => {
            socket.write(Buffer.from(payload, 'latin1'))
          }
          const socket = tls
            ? tlsMod.connect({ host, port, rejectUnauthorized: false, servername: host }, onConnect)
            : net.connect({ host, port }, onConnect)
          const to = setTimeout(() => {
            socket.destroy()
            // Return what we have rather than failing — short responses may not FIN.
            resolve(Buffer.concat(chunks))
          }, timeoutMs ?? 8000)
          socket.on('data', (c: Buffer) => chunks.push(c))
          socket.on('end', () => {
            clearTimeout(to)
            resolve(Buffer.concat(chunks))
          })
          socket.on('error', (e) => {
            clearTimeout(to)
            reject(e)
          })
        })
        if (result.length === 0) return err('no response (connection made, nothing returned before timeout)')
        const parsed = parseRawResponse(result)
        return ok(
          JSON.stringify(
            {
              sent: payload.split('\r\n')[0],
              statusLine: parsed.statusLine,
              status: parsed.status,
              headers: parsed.headers,
              bodyPreview: parsed.body.slice(0, 1200),
              bodyBytes: parsed.body.length
            },
            null,
            2
          )
        )
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
