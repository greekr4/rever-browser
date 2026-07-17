import { session } from 'electron'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { pbkdf2Sync, createDecipheriv } from 'node:crypto'
import { copyFileSync, existsSync, rmSync, readdirSync, statSync, mkdtempSync, mkdirSync, chmodSync } from 'node:fs'
import { tmpdir, homedir } from 'node:os'
import { join } from 'node:path'

import { getActivePartition } from './tab-partition'

// Import session cookies from the real desktop Chrome profile into the active
// tab's partition. This is the single biggest lever for WAF / CAPTCHA
// avoidance: a brand-new partition with zero cookies looks like a first-time
// visitor and gets the strictest treatment. Re-using a logged-in Chrome
// session makes us look like a returning, trusted user.
//
// macOS only. Chrome cookie values are AES-128-CBC encrypted with a key
// derived (PBKDF2-SHA1) from the "Chrome Safe Storage" password in the login
// Keychain. Recent Chrome prepends a 32-byte SHA256(host) to the plaintext —
// detected and stripped adaptively below. App-bound `v20` values (newest
// Chrome) cannot be decrypted via the Keychain and are reported, not faked.

const execFileAsync = promisify(execFile)

const IV = Buffer.alloc(16, 0x20) // 16 spaces
const DOMAIN_HASH_LEN = 32
// Chrome stores expires_utc as microseconds since 1601-01-01 (Windows epoch).
const WINDOWS_EPOCH_OFFSET_SECONDS = 11_644_473_600

export interface ChromeImportOptions {
  profile?: string
  // Substring filters on the cookie host (e.g. ['yes24.com', 'google']). When
  // omitted, every cookie in the profile is imported.
  hosts?: string[]
}

export interface ChromeImportResult {
  ok: boolean
  imported: number
  skipped: number
  undecryptable: number
  total: number
  error?: string
}

interface RawCookie {
  host: string
  name: string
  path: string | null
  secure: number
  httpOnly: number
  expires: number
  enc: string // hex
}

function chromeBaseDir(): string {
  return join(homedir(), 'Library', 'Application Support', 'Google', 'Chrome')
}

export function listChromeProfiles(): string[] {
  if (process.platform !== 'darwin') return []
  const base = chromeBaseDir()
  if (!existsSync(base)) return []
  try {
    return readdirSync(base).filter((name) => {
      try {
        return statSync(join(base, name)).isDirectory() && existsSync(join(base, name, 'Cookies'))
      } catch {
        return false
      }
    })
  } catch {
    return []
  }
}

async function getSafeStorageKey(): Promise<Buffer> {
  // Triggers a one-time macOS Keychain access prompt the user must allow.
  const { stdout } = await execFileAsync('security', ['find-generic-password', '-wa', 'Chrome'])
  const password = stdout.trim()
  if (!password) throw new Error('empty Chrome Safe Storage password')
  return pbkdf2Sync(password, 'saltysalt', 1003, 16, 'sha1')
}

function isMostlyPrintable(buf: Buffer): boolean {
  if (buf.length === 0) return true
  let printable = 0
  for (const b of buf) if (b >= 0x20 && b < 0x7f) printable++
  return printable / buf.length > 0.85
}

// Returns the decrypted value, or null when the blob is app-bound (`v20`) or
// otherwise undecryptable via the Keychain.
function decryptV10(encrypted: Buffer, key: Buffer): string | null {
  if (encrypted.length <= 3) return null
  const prefix = encrypted.subarray(0, 3).toString('latin1')
  if (prefix !== 'v10') return null // v20 = app-bound; can't decrypt with the Keychain key
  const ct = encrypted.subarray(3)
  if (ct.length === 0 || ct.length % 16 !== 0) return null
  try {
    const decipher = createDecipheriv('aes-128-cbc', key, IV)
    decipher.setAutoPadding(true)
    let pt = Buffer.concat([decipher.update(ct), decipher.final()])
    // Strip the SHA256(host) prefix when present (modern Chrome). The hash is
    // 32 random bytes (non-printable); a bare value starts printable.
    if (pt.length >= DOMAIN_HASH_LEN && !isMostlyPrintable(pt.subarray(0, DOMAIN_HASH_LEN))) {
      pt = pt.subarray(DOMAIN_HASH_LEN)
    }
    return pt.toString('utf8')
  } catch {
    return null
  }
}

async function readEncryptedCookies(profile: string): Promise<RawCookie[]> {
  const base = chromeBaseDir()
  const srcDir = join(base, profile)
  const src = join(srcDir, 'Cookies')
  if (!existsSync(src)) throw new Error(`no Cookies db for profile "${profile}"`)

  // Copy the db (+ WAL sidecars) so we read a consistent snapshot while Chrome
  // holds the live file locked.
  // 0700 전용 디렉터리에 복사해 다른 사용자가 쿠키 파일을 읽지 못하게 한다.
  const tmpDir = mkdtempSync(join(tmpdir(), 'rev-chrome-cookies-'))
  try { mkdirSync(tmpDir, { recursive: true }); chmodSync(tmpDir, 0o700) } catch {}
  const tmp = join(tmpDir, 'Cookies.db')
  const cleanup: string[] = [tmpDir]
  copyFileSync(src, tmp)
  for (const ext of ['-wal', '-shm']) {
    if (existsSync(src + ext)) {
      copyFileSync(src + ext, tmp + ext)
      // cleanup은 tmpDir 통째로 삭제하므로 개별 파일은 불필요
    }
  }

  try {
    const { stdout } = await execFileAsync(
      'sqlite3',
      [
        '-json',
        tmp,
        'SELECT host_key AS host, name, path, is_secure AS secure, is_httponly AS httpOnly, ' +
          'expires_utc AS expires, hex(encrypted_value) AS enc FROM cookies;'
      ],
      { maxBuffer: 128 * 1024 * 1024 }
    )
    const trimmed = stdout.trim()
    return trimmed ? (JSON.parse(trimmed) as RawCookie[]) : []
  } finally {
    for (const f of cleanup) rmSync(f, { force: true, recursive: true })
  }
}

export async function importChromeCookies(
  opts: ChromeImportOptions = {}
): Promise<ChromeImportResult> {
  const empty: ChromeImportResult = { ok: false, imported: 0, skipped: 0, undecryptable: 0, total: 0 }
  if (process.platform !== 'darwin') {
    return { ...empty, error: 'Chrome cookie import is macOS-only for now.' }
  }

  const profile = opts.profile || 'Default'
  const hostFilters = (opts.hosts ?? []).map((h) => h.toLowerCase()).filter(Boolean)

  // 빈 host 필터는 전체 Chrome 쿠키를 앱에 주입한다. 이는 보안상 위험하지만
  // 기존 IPC/UI 호출부를 깨지 않기 위해 에러 대신 경고 로그만 남긴다.
  // 향후 UI에서 명시적 확인 절차를 추가할 것을 권장한다.
  if (hostFilters.length === 0) {
    console.warn('[chrome-cookie-import] WARNING: no host filter specified — importing ALL cookies from profile. This is a security risk.')
  }

  let key: Buffer
  try {
    key = await getSafeStorageKey()
  } catch (e) {
    return {
      ...empty,
      error: `Keychain access failed (allow "Chrome Safe Storage" when prompted): ${e instanceof Error ? e.message : String(e)}`
    }
  }

  let raw: RawCookie[]
  try {
    raw = await readEncryptedCookies(profile)
  } catch (e) {
    return { ...empty, error: e instanceof Error ? e.message : String(e) }
  }

  const matched = hostFilters.length
    ? raw.filter((c) => hostFilters.some((f) => c.host.toLowerCase().includes(f)))
    : raw

  const sess = session.fromPartition(getActivePartition())
  const nowSec = Date.now() / 1000
  let imported = 0
  let skipped = 0
  let undecryptable = 0

  for (const c of matched) {
    const value = decryptV10(Buffer.from(c.enc, 'hex'), key)
    if (value == null) {
      undecryptable++
      continue
    }
    const expiresSec = c.expires > 0 ? c.expires / 1e6 - WINDOWS_EPOCH_OFFSET_SECONDS : 0
    if (expiresSec > 0 && expiresSec <= nowSec) {
      skipped++ // already expired
      continue
    }
    const isDomainCookie = c.host.startsWith('.')
    const cookieHost = c.host.replace(/^\./, '')
    const cookiePath = c.path || '/'
    const url = `${c.secure ? 'https' : 'http'}://${cookieHost}${cookiePath}`
    try {
      await sess.cookies.set({
        url,
        name: c.name,
        value,
        path: cookiePath,
        secure: !!c.secure,
        httpOnly: !!c.httpOnly,
        ...(isDomainCookie ? { domain: c.host } : {}),
        ...(expiresSec > 0 ? { expirationDate: expiresSec } : {})
      })
      imported++
    } catch {
      skipped++
    }
  }

  return { ok: true, imported, skipped, undecryptable, total: matched.length }
}
