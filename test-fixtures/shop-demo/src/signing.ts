// The reversing target: how the price API request is signed. A reverser must
// recover exactly this from the shipped bundle, so it lives in its own module.

export const HMAC_KEY = 'nile-price-signing-key-2026'
export const API_BASE = '/api'

const enc = new TextEncoder()

export function toHex(bytes: Uint8Array): string {
  return [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('')
}

export async function hmacSha256(key: string, message: string): Promise<string> {
  const k = await crypto.subtle.importKey(
    'raw',
    enc.encode(key),
    { name: 'HMAC', hash: 'SHA-256' },
    false,
    ['sign']
  )
  const sig = await crypto.subtle.sign('HMAC', k, enc.encode(message))
  return toHex(new Uint8Array(sig))
}

/** The canonical string the server also builds: METHOD\nPATH\nBODY\nTIMESTAMP */
export function canonicalString(method: string, path: string, body: string, ts: string): string {
  return `${method}\n${path}\n${body}\n${ts}`
}

export async function signedHeaders(
  method: string,
  path: string,
  body: string,
  token: string
): Promise<Record<string, string>> {
  const ts = String(Date.now())
  const sig = await hmacSha256(HMAC_KEY, canonicalString(method, path, body, ts))
  return {
    authorization: `Bearer ${token}`,
    'x-timestamp': ts,
    'x-signature': sig,
    'content-type': 'application/json'
  }
}
