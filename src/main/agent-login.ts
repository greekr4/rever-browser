import { execFile } from 'node:child_process'
import { readFile } from 'node:fs/promises'
import { homedir, platform } from 'node:os'
import { join } from 'node:path'
import { promisify } from 'node:util'

const execFileP = promisify(execFile)

// Detects agent CLIs the user is *already* signed in to, so onboarding can show
// "connected automatically" instead of asking for an API key they don't need.
// Credentials are only inspected for shape — tokens are never logged or
// returned across IPC.

export type LoginAgentId = 'claude-code' | 'codex'

export interface LoginInfo {
  loggedIn: boolean
  /** Plan/mode the credential advertises ('max', 'chatgpt', 'api-key'), or null. */
  plan: string | null
}

const NO_LOGIN: LoginInfo = { loggedIn: false, plan: null }

function parse(raw: string): Record<string, unknown> | null {
  try {
    const v = JSON.parse(raw)
    return v && typeof v === 'object' ? (v as Record<string, unknown>) : null
  } catch {
    return null
  }
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

/**
 * Read a Claude Code credential blob (keychain item or ~/.claude/.credentials.json).
 * An expired access token still counts as logged in while the refresh token is
 * alive — the CLI refreshes on its own.
 */
export function readClaudeLogin(raw: string, now: number): LoginInfo {
  const oauth = parse(raw)?.claudeAiOauth as Record<string, unknown> | undefined
  if (!oauth || typeof oauth !== 'object') return NO_LOGIN
  if (!str(oauth.accessToken)) return NO_LOGIN

  const plan = str(oauth.subscriptionType) || null
  const expiresAt = typeof oauth.expiresAt === 'number' ? oauth.expiresAt : null
  if (expiresAt === null || expiresAt > now) return { loggedIn: true, plan }

  // Access token expired — fall back to the refresh token.
  if (!str(oauth.refreshToken)) return NO_LOGIN
  const refreshExpiresAt =
    typeof oauth.refreshTokenExpiresAt === 'number' ? oauth.refreshTokenExpiresAt : null
  if (refreshExpiresAt !== null && refreshExpiresAt <= now) return NO_LOGIN
  return { loggedIn: true, plan }
}

/** Read a Codex CLI ~/.codex/auth.json blob. */
export function readCodexLogin(raw: string): LoginInfo {
  const root = parse(raw)
  if (!root) return NO_LOGIN

  const tokens = root.tokens as Record<string, unknown> | undefined
  if (tokens && str(tokens.access_token)) {
    return { loggedIn: true, plan: str(root.auth_mode) || null }
  }
  if (str(root.OPENAI_API_KEY)) return { loggedIn: true, plan: 'api-key' }
  return NO_LOGIN
}

async function readFileOrNull(path: string): Promise<string | null> {
  try {
    return await readFile(path, 'utf8')
  } catch {
    return null
  }
}

/**
 * macOS stores the Claude credential in the login keychain; other platforms
 * keep it in ~/.claude/.credentials.json. Try the keychain first, then the file.
 */
async function readClaudeCredentialBlob(): Promise<string | null> {
  if (platform() === 'darwin') {
    try {
      const { stdout } = await execFileP(
        'security',
        ['find-generic-password', '-s', 'Claude Code-credentials', '-w'],
        { timeout: 3_000 }
      )
      const trimmed = stdout.trim()
      if (trimmed) return trimmed
    } catch {
      // Not in the keychain (or access denied) — fall through to the file.
    }
  }
  return readFileOrNull(join(homedir(), '.claude', '.credentials.json'))
}

/** Probe every agent that can be signed in via its own CLI. */
export async function detectAgentLogins(): Promise<Record<LoginAgentId, LoginInfo>> {
  const [claudeRaw, codexRaw] = await Promise.all([
    readClaudeCredentialBlob(),
    readFileOrNull(join(homedir(), '.codex', 'auth.json'))
  ])
  return {
    'claude-code': claudeRaw ? readClaudeLogin(claudeRaw, Date.now()) : NO_LOGIN,
    codex: codexRaw ? readCodexLogin(codexRaw) : NO_LOGIN
  }
}
