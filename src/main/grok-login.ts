import { readFileSync, renameSync, statSync, writeFileSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { join } from 'node:path'

import type { LoginInfo } from './agent-login'

// grok.com subscription stored by `grok login` at ~/.grok/auth.json.
// The OIDC access token is a bearer for https://api.x.ai/v1. The token never
// crosses IPC; callers in the main process read it here.

const NO_LOGIN: LoginInfo = { loggedIn: false, plan: null }
export const GROK_PLAN = 'grok.com'
const REFRESH_SKEW_MS = 60_000

export function grokAuthPath(): string {
  return join(homedir(), '.grok', 'auth.json')
}

interface GrokAuthEntry {
  key?: string
  refresh_token?: string
  expires_at?: string
  auth_mode?: string
  oidc_client_id?: string
  oidc_issuer?: string
}

interface TokenResponse {
  access_token?: string
  refresh_token?: string
  expires_in?: number
}

function parseRoot(raw: string): Record<string, GrokAuthEntry> | null {
  try {
    const value = JSON.parse(raw) as unknown
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null
    return value as Record<string, GrokAuthEntry>
  } catch {
    return null
  }
}

function pickEntry(root: Record<string, GrokAuthEntry>): { id: string; entry: GrokAuthEntry } | null {
  const rows = Object.entries(root).filter(([, entry]) => !!entry && typeof entry === 'object')
  const oidc = rows.find(([, entry]) => entry.auth_mode === 'oidc' && (entry.key || entry.refresh_token))
  const any = rows.find(([, entry]) => entry.key || entry.refresh_token)
  const hit = oidc ?? any
  return hit ? { id: hit[0], entry: hit[1] } : null
}

function text(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

/** Shape check only. A live refresh token still counts after the access token expires. */
export function readGrokLogin(raw: string, now: number): LoginInfo {
  const root = parseRoot(raw)
  if (!root) return NO_LOGIN
  const picked = pickEntry(root)
  if (!picked) return NO_LOGIN
  const key = text(picked.entry.key)
  const refresh = text(picked.entry.refresh_token)
  if (!key && !refresh) return NO_LOGIN
  if (!key) return { loggedIn: true, plan: GROK_PLAN }
  const expiresAt = Date.parse(text(picked.entry.expires_at))
  if (Number.isNaN(expiresAt) || expiresAt > now) return { loggedIn: true, plan: GROK_PLAN }
  if (refresh) return { loggedIn: true, plan: GROK_PLAN }
  return NO_LOGIN
}

export function readGrokLoginFile(now = Date.now()): LoginInfo {
  try {
    return readGrokLogin(readFileSync(grokAuthPath(), 'utf8'), now)
  } catch {
    return NO_LOGIN
  }
}

function usableKey(entry: GrokAuthEntry, now: number): string | null {
  const key = text(entry.key)
  if (!key) return null
  const expiresAt = Date.parse(text(entry.expires_at))
  if (!Number.isNaN(expiresAt) && expiresAt - REFRESH_SKEW_MS <= now) return null
  return key
}

/** Merge a token response into the auth file contents. Returns null when nothing usable came back. */
export function applyGrokTokenResponse(
  raw: string,
  token: TokenResponse,
  now: number
): { raw: string; accessToken: string } | null {
  if (!token.access_token) return null
  const root = parseRoot(raw)
  if (!root) return null
  const picked = pickEntry(root)
  if (!picked) return null
  const entry = root[picked.id]
  entry.key = token.access_token
  if (token.refresh_token) entry.refresh_token = token.refresh_token
  if (typeof token.expires_in === 'number' && token.expires_in > 0) {
    entry.expires_at = new Date(now + token.expires_in * 1000).toISOString()
  }
  const pretty = raw.includes('\n')
  const next = pretty ? `${JSON.stringify(root, null, 2)}\n` : JSON.stringify(root)
  return { raw: next, accessToken: token.access_token }
}

async function refreshEntry(entry: GrokAuthEntry): Promise<TokenResponse | null> {
  const refresh = text(entry.refresh_token)
  const clientId = text(entry.oidc_client_id)
  if (!refresh || !clientId) return null
  const issuer = text(entry.oidc_issuer) || 'https://auth.x.ai'
  const response = await fetch(`${issuer.replace(/\/$/, '')}/oauth2/token`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      grant_type: 'refresh_token',
      refresh_token: refresh,
      client_id: clientId
    })
  })
  if (!response.ok) return null
  return (await response.json()) as TokenResponse
}

function writeAuth(next: string): void {
  const path = grokAuthPath()
  const mode = statSync(path).mode & 0o777
  const tmp = join(tmpdir(), `.grok-auth-${process.pid}.json`)
  writeFileSync(tmp, next, { mode })
  renameSync(tmp, path)
}

/** Access token from `grok login`, refreshing it when it is inside the skew window. */
export async function ensureGrokAccessToken(now = Date.now()): Promise<string | null> {
  let raw: string
  try {
    raw = readFileSync(grokAuthPath(), 'utf8')
  } catch {
    return null
  }
  const root = parseRoot(raw)
  if (!root) return null
  const picked = pickEntry(root)
  if (!picked) return null
  const current = usableKey(picked.entry, now)
  if (current) return current

  const token = await refreshEntry(picked.entry)
  if (!token) return null
  const applied = applyGrokTokenResponse(raw, token, now)
  if (!applied) return null
  try {
    writeAuth(applied.raw)
  } catch {
    // Keep serving the new access token for this process.
  }
  return applied.accessToken
}

export async function resolveGrokCredential(
  storedKey: string | null
): Promise<{ token: string; source: 'subscription' | 'api-key' } | null> {
  try {
    const oauth = await ensureGrokAccessToken()
    if (oauth) return { token: oauth, source: 'subscription' }
  } catch {
    // Fall through to a pasted key or XAI_API_KEY.
  }
  if (storedKey) return { token: storedKey, source: 'api-key' }
  return null
}
