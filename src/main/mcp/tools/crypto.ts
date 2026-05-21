import { z } from 'zod'
import { createHash, createHmac } from 'node:crypto'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { ok, err, errorMessage } from '../utils'

type Transform =
  | 'base64-encode'
  | 'base64-decode'
  | 'base64url-encode'
  | 'base64url-decode'
  | 'hex-encode'
  | 'hex-decode'
  | 'url-encode'
  | 'url-decode'
  | 'reverse'
  | 'rot13'
  | 'utf8-encode'
  | 'utf8-decode'
  | 'json-stringify'
  | 'json-parse'

function rot13(s: string): string {
  return s.replace(/[a-zA-Z]/g, (c) => {
    const code = c.charCodeAt(0)
    const base = code >= 97 ? 97 : 65
    return String.fromCharCode(((code - base + 13) % 26) + base)
  })
}

function apply(input: string, t: Transform): string {
  switch (t) {
    case 'base64-encode':
      return Buffer.from(input, 'utf8').toString('base64')
    case 'base64-decode':
      return Buffer.from(input, 'base64').toString('utf8')
    case 'base64url-encode':
      return Buffer.from(input, 'utf8').toString('base64url')
    case 'base64url-decode':
      return Buffer.from(input, 'base64url').toString('utf8')
    case 'hex-encode':
      return Buffer.from(input, 'utf8').toString('hex')
    case 'hex-decode':
      return Buffer.from(input, 'hex').toString('utf8')
    case 'url-encode':
      return encodeURIComponent(input)
    case 'url-decode':
      return decodeURIComponent(input)
    case 'reverse':
      return input.split('').reverse().join('')
    case 'rot13':
      return rot13(input)
    case 'utf8-encode':
      return Buffer.from(input).toString('utf8')
    case 'utf8-decode':
      return Buffer.from(input, 'utf8').toString()
    case 'json-stringify':
      return JSON.stringify(input)
    case 'json-parse':
      return String(JSON.parse(input))
  }
}

// Well-known magic-hash collisions for PHP `==` / `md5(...,true)` SQLi tricks.
const MAGIC_HASH_SAMPLES = {
  md5: [
    { value: '240610708', hash: '0e462097431906509019562988736854' },
    { value: 'QNKCDZO', hash: '0e830400451993494058024219903391' },
    { value: 'aabg7XSs', hash: '0e087386482136013740957780965295' },
    { value: 'aabC9RqS', hash: '0e041022518165728065344349536299' }
  ],
  // For md5(...,true) raw-bytes SQLi (`' or '6É]\x1f\x...` patterns)
  rawMd5SQLi: [
    { value: 'ffifdyop', note: "md5(ffifdyop, true) contains \"'or'6\\xc2\\xa2\" — bypasses ' OR ' filter" }
  ],
  sha1: [
    { value: '10932435112', hash: '0e07766915004133176347055865026311692244' }
  ]
}

export function registerCryptoTools(mcp: McpServer) {
  mcp.registerTool(
    'crypto_chain',
    {
      description:
        'Apply a chain of encoding/decoding transforms to a string. Useful for decoding nested base64/hex/url/rot13 cookie values or reverse-engineering custom obfuscation.',
      inputSchema: {
        input: z.string().describe('Input string'),
        transforms: z
          .array(
            z.enum([
              'base64-encode',
              'base64-decode',
              'base64url-encode',
              'base64url-decode',
              'hex-encode',
              'hex-decode',
              'url-encode',
              'url-decode',
              'reverse',
              'rot13',
              'utf8-encode',
              'utf8-decode',
              'json-stringify',
              'json-parse'
            ])
          )
          .describe('Pipeline of transforms applied in order')
      }
    },
    async ({ input, transforms }) => {
      try {
        const steps: Array<{ transform: string; result: string }> = []
        let cur = input
        for (const t of transforms) {
          cur = apply(cur, t as Transform)
          steps.push({ transform: t, result: cur.length > 200 ? cur.slice(0, 200) + '…' : cur })
        }
        return ok(JSON.stringify({ input, steps, final: cur }, null, 2))
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )

  mcp.registerTool(
    'hash_iter',
    {
      description:
        'Iterate a hash N times. Used for challenges that require finding a string whose nested hash matches a target (e.g. webhacking.kr SHA1 x 500).',
      inputSchema: {
        input: z.string().describe('Initial string'),
        algorithm: z.enum(['md5', 'sha1', 'sha256', 'sha512']).describe('Hash algorithm'),
        iterations: z.number().int().positive().max(100000).describe('Number of times to hash'),
        encoding: z.enum(['hex', 'base64']).optional().describe('Output encoding (default hex)')
      }
    },
    async ({ input, algorithm, iterations, encoding = 'hex' }) => {
      let cur = input
      for (let i = 0; i < iterations; i++) {
        cur = createHash(algorithm).update(cur).digest(encoding as 'hex' | 'base64')
      }
      return ok(JSON.stringify({ algorithm, iterations, encoding, result: cur }, null, 2))
    }
  )

  mcp.registerTool(
    'hmac_compute',
    {
      description: 'Compute HMAC of a message with a key (for API signature reversing).',
      inputSchema: {
        message: z.string(),
        key: z.string(),
        algorithm: z.enum(['md5', 'sha1', 'sha256', 'sha512']),
        encoding: z.enum(['hex', 'base64']).optional()
      }
    },
    async ({ message, key, algorithm, encoding = 'hex' }) => {
      const mac = createHmac(algorithm, key).update(message).digest(encoding as 'hex' | 'base64')
      return ok(JSON.stringify({ algorithm, encoding, result: mac }, null, 2))
    }
  )

  mcp.registerTool(
    'magic_hash_lookup',
    {
      description:
        'Return known "magic hash" values that PHP loose-comparison or md5(...,true) SQLi vulnerabilities accept (e.g. 240610708 hashes to "0e...all digits").',
      inputSchema: {
        algorithm: z.enum(['md5', 'sha1', 'rawMd5SQLi']).optional().describe('Algorithm filter')
      }
    },
    async ({ algorithm }) => {
      const out = algorithm ? { [algorithm]: MAGIC_HASH_SAMPLES[algorithm] } : MAGIC_HASH_SAMPLES
      return ok(JSON.stringify(out, null, 2))
    }
  )
}
