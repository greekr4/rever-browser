// Pure token detection/decoding, electron-free so it is unit-tested directly.

export function decodeBase64Url(s: string): string {
  const padded = s
    .replace(/-/g, '+')
    .replace(/_/g, '/')
    .padEnd(s.length + ((4 - (s.length % 4)) % 4), '=')
  return Buffer.from(padded, 'base64').toString('utf8')
}

export function tryParseJson(s: string): unknown | undefined {
  try {
    return JSON.parse(s)
  } catch {
    return undefined
  }
}

export function formatExpiry(payload: unknown): string {
  if (typeof payload !== 'object' || payload === null) return ''
  const p = payload as Record<string, unknown>
  const exp = p['exp']
  if (typeof exp !== 'number') return ''
  const d = new Date(exp * 1000)
  const expired = d.getTime() < Date.now()
  return ` (exp: ${d.toISOString()}${expired ? ' — EXPIRED' : ''})`
}

/** Detect and decode a token value. Returns a plain object describing the result. */
export function decodeToken(value: string): Record<string, unknown> {
  const v = value.trim()

  // JWT: 3 base64url segments separated by dots.
  const jwtParts = v.split('.')
  if (jwtParts.length === 3) {
    try {
      const header = tryParseJson(decodeBase64Url(jwtParts[0]))
      const payload = tryParseJson(decodeBase64Url(jwtParts[1]))
      if (header && payload) {
        return {
          type: 'jwt',
          header,
          payload,
          note: `signature not verified${formatExpiry(payload)}`,
          raw: v
        }
      }
    } catch {
      // fall through
    }
  }

  // URL-encoded JSON.
  if (v.includes('%')) {
    try {
      const decoded = decodeURIComponent(v)
      const parsed = tryParseJson(decoded)
      if (parsed) return { type: 'url-encoded-json', decoded: parsed, raw: v }
      return { type: 'url-encoded', decoded, raw: v }
    } catch {
      // fall through
    }
  }

  // base64 (eyJ prefix is common for JSON).
  if (/^[A-Za-z0-9+/=_-]{4,}$/.test(v)) {
    try {
      const decoded = decodeBase64Url(v)
      const parsed = tryParseJson(decoded)
      if (parsed) return { type: 'base64-json', decoded: parsed, raw: v }
      if (/^[\x20-\x7E\t\n\r]+$/.test(decoded)) return { type: 'base64', decoded, raw: v }
    } catch {
      // fall through
    }
  }

  // hex.
  if (/^[0-9a-fA-F]{8,}$/.test(v) && v.length % 2 === 0) {
    return { type: 'hex', decoded: Buffer.from(v, 'hex').toString('utf8'), raw: v }
  }

  return { type: 'unknown', note: 'Could not detect encoding', raw: v }
}
