import http2 from 'node:http2'

import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { ok, err, errorMessage } from '../utils'

// Minimal gRPC client — rever is otherwise HTTP/browser-only and cannot reach a
// gRPC backend (e.g. a service the web front calls internally on another port).
// Encodes a small protobuf message from scalar field specs, frames it in the
// gRPC length-prefix, sends it over HTTP/2 (h2c plaintext by default for local
// targets), and reports the gRPC status + response bytes. Scope: scalar fields
// (string/int/bool/bytes) by field number — enough for CTF-style messages where
// the .proto is known (read it from the captured scripts or the repo).

export type FieldType = 'string' | 'int' | 'bool' | 'bytes'
export interface ProtoField {
  number: number
  type: FieldType
  value: string | number | boolean
}

export function encodeVarint(n: number): Buffer {
  if (n < 0) throw new Error('varint must be non-negative')
  const bytes: number[] = []
  let v = n
  do {
    let b = v & 0x7f
    v = Math.floor(v / 128)
    if (v > 0) b |= 0x80
    bytes.push(b)
  } while (v > 0)
  return Buffer.from(bytes)
}

export function encodeField(f: ProtoField): Buffer {
  if (!Number.isInteger(f.number) || f.number < 1) throw new Error(`bad field number: ${f.number}`)
  if (f.type === 'string' || f.type === 'bytes') {
    const payload =
      f.type === 'string' ? Buffer.from(String(f.value), 'utf8') : Buffer.from(String(f.value), 'base64')
    const key = encodeVarint((f.number << 3) | 2) // wire type 2: length-delimited
    return Buffer.concat([key, encodeVarint(payload.length), payload])
  }
  // wire type 0: varint (int / bool)
  const key = encodeVarint((f.number << 3) | 0)
  const num = f.type === 'bool' ? (f.value ? 1 : 0) : Number(f.value)
  if (!Number.isInteger(num) || num < 0) throw new Error(`int field ${f.number} must be a non-negative integer`)
  return Buffer.concat([key, encodeVarint(num)])
}

export function encodeMessage(fields: ProtoField[]): Buffer {
  return Buffer.concat(fields.map(encodeField))
}

/** gRPC length-prefixed frame: 1 compression byte (0) + 4-byte BE length + message. */
export function frameMessage(msg: Buffer): Buffer {
  const head = Buffer.alloc(5)
  head.writeUInt8(0, 0)
  head.writeUInt32BE(msg.length, 1)
  return Buffer.concat([head, msg])
}

/** Strip the 5-byte gRPC prefix from a response DATA payload (first frame). */
export function unframeMessage(buf: Buffer): Buffer {
  if (buf.length < 5) return Buffer.alloc(0)
  const len = buf.readUInt32BE(1)
  return buf.subarray(5, 5 + len)
}

/** Pull length-delimited (wire type 2) field values out of a protobuf buffer as
 *  UTF-8 — a loose decode so the caller can read string reply fields. */
export function extractStringFields(buf: Buffer): Record<number, string> {
  const out: Record<number, string> = {}
  let i = 0
  const readVarint = (): number => {
    let shift = 0
    let result = 0
    while (i < buf.length) {
      const b = buf[i++]
      result += (b & 0x7f) * Math.pow(2, shift)
      if ((b & 0x80) === 0) break
      shift += 7
    }
    return result
  }
  while (i < buf.length) {
    const key = readVarint()
    const field = key >> 3
    const wire = key & 0x7
    if (wire === 2) {
      const len = readVarint()
      out[field] = buf.subarray(i, i + len).toString('utf8')
      i += len
    } else if (wire === 0) {
      readVarint()
    } else if (wire === 5) {
      i += 4
    } else if (wire === 1) {
      i += 8
    } else break
  }
  return out
}

export function registerGrpcCallTools(mcp: McpServer) {
  mcp.registerTool(
    'grpc_call',
    {
      description:
        'Call a gRPC method directly over HTTP/2 — reaches a backend gRPC service the HTTP front talks to internally (another port), which browser/HTTP tools cannot. Encodes a protobuf message from scalar field specs (string/int/bool/bytes by field number — read the .proto for numbers), frames it, sends it, and returns the gRPC status + decoded reply strings. Use when you find a gRPC port (e.g. :50045) or a .proto and need to hit the raw service — notably to bypass an HTTP-layer input filter that the gRPC server itself does not enforce (unsanitized filenames, path traversal, etc.). Plaintext h2c by default (tls:true for TLS).',
      inputSchema: {
        target: z.string().describe('host:port of the gRPC server, e.g. "127.0.0.1:50045"'),
        path: z.string().describe('gRPC path: /<package.>Service/Method, e.g. "/RickyService/SubmitTestimonial"'),
        fields: z
          .array(
            z.object({
              number: z.number().int().describe('protobuf field number (from the .proto)'),
              type: z.enum(['string', 'int', 'bool', 'bytes']),
              value: z.union([z.string(), z.number(), z.boolean()])
            })
          )
          .describe('scalar fields of the request message'),
        tls: z.boolean().optional().describe('Use TLS (default false = plaintext h2c, right for local targets)'),
        timeoutMs: z.number().int().optional().describe('default 8000')
      }
    },
    async ({ target, path, fields, tls, timeoutMs }) => {
      try {
        const body = frameMessage(encodeMessage(fields as ProtoField[]))
        const authority = `${tls ? 'https' : 'http'}://${target}`
        const result = await new Promise<{ status: string; message: string; data: Buffer; httpStatus?: number }>(
          (resolve, reject) => {
            const client = http2.connect(authority, tls ? {} : {})
            const to = setTimeout(() => {
              client.destroy()
              reject(new Error(`gRPC call timed out after ${timeoutMs ?? 8000}ms`))
            }, timeoutMs ?? 8000)
            client.on('error', (e) => {
              clearTimeout(to)
              reject(e)
            })
            const req = client.request({
              ':method': 'POST',
              ':path': path,
              'content-type': 'application/grpc',
              te: 'trailers'
            })
            let httpStatus: number | undefined
            let grpcStatus = ''
            let grpcMessage = ''
            const chunks: Buffer[] = []
            req.on('response', (h) => {
              httpStatus = Number(h[':status'])
              if (h['grpc-status'] != null) grpcStatus = String(h['grpc-status'])
              if (h['grpc-message'] != null) grpcMessage = decodeURIComponent(String(h['grpc-message']))
            })
            req.on('trailers', (t) => {
              if (t['grpc-status'] != null) grpcStatus = String(t['grpc-status'])
              if (t['grpc-message'] != null) grpcMessage = decodeURIComponent(String(t['grpc-message']))
            })
            req.on('data', (c: Buffer) => chunks.push(c))
            req.on('end', () => {
              clearTimeout(to)
              client.close()
              resolve({ status: grpcStatus, message: grpcMessage, data: Buffer.concat(chunks), httpStatus })
            })
            req.on('error', (e) => {
              clearTimeout(to)
              reject(e)
            })
            req.end(body)
          }
        )
        const reply = unframeMessage(result.data)
        return ok(
          JSON.stringify(
            {
              httpStatus: result.httpStatus,
              grpcStatus: result.status === '' ? '(none)' : result.status,
              grpcStatusOk: result.status === '0' || result.status === '',
              grpcMessage: result.message,
              replyStrings: extractStringFields(reply),
              replyHex: reply.toString('hex').slice(0, 256)
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
