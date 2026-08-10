import { session } from 'electron'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { pbkdf2Sync, createDecipheriv } from 'node:crypto'
import {
  copyFileSync,
  existsSync,
  rmSync,
  readdirSync,
  readFileSync,
  statSync,
  mkdtempSync,
  mkdirSync,
  chmodSync
} from 'node:fs'
import { tmpdir, homedir } from 'node:os'
import { join } from 'node:path'

import { getActivePartition } from './tab-partition'

// Import session cookies from a real desktop browser into the active tab's
// partition. Re-using a logged-in session is the single biggest lever for WAF /
// CAPTCHA avoidance: a fresh partition looks like a first-time visitor and gets
// the strictest treatment; imported cookies make us a returning, trusted user.
//
// macOS only. Three cookie backends:
//  - Chromium family (Chrome/Edge/Brave/Arc/Chromium/Vivaldi): a SQLite db with
//    AES-128-CBC `v10` values, keyed by PBKDF2-SHA1 over the browser's "Safe
//    Storage" Keychain password. `v20` (app-bound) values can't be decrypted
//    via the Keychain and are reported, not faked.
//  - Firefox: a SQLite db (moz_cookies) with plaintext values — no Keychain.
//  - Safari: the proprietary Cookies.binarycookies format (needs Full Disk
//    Access; a permission error yields a clear hint).

const execFileAsync = promisify(execFile)

const IV = Buffer.alloc(16, 0x20) // 16 spaces
const DOMAIN_HASH_LEN = 32
// Chromium stores expires_utc as microseconds since 1601-01-01 (Windows epoch).
const WINDOWS_EPOCH_OFFSET_SECONDS = 11_644_473_600
// Safari/Cocoa store timestamps as seconds since 2001-01-01.
const COCOA_EPOCH_OFFSET_SECONDS = 978_307_200

export type BrowserId =
  | 'chrome'
  | 'edge'
  | 'brave'
  | 'arc'
  | 'chromium'
  | 'vivaldi'
  | 'firefox'
  | 'safari'

export interface BrowserProfileInfo {
  // Directory identifier used to locate the cookie db (e.g. 'Default', 'Profile 1').
  id: string
  // Human-facing display name (e.g. '태균', 'vercel'); falls back to the id.
  name: string
}

export interface BrowserInfo {
  id: BrowserId
  name: string
  // Profiles to import from. Chromium reads display names from Local State;
  // Firefox uses profile dir names; Safari has a single implicit profile.
  profiles: BrowserProfileInfo[]
}

export interface ImportOptions {
  browser: BrowserId
  profile?: string
  // Substring filters on the cookie host (e.g. ['yes24.com', 'google']). When
  // omitted, every cookie is imported.
  hosts?: string[]
}

export interface ImportResult {
  ok: boolean
  imported: number
  skipped: number
  undecryptable: number
  total: number
  error?: string
}

// A normalized cookie ready to be written to the Electron session.
interface Cookie {
  host: string
  name: string
  value: string
  path: string
  secure: boolean
  httpOnly: boolean
  // Unix seconds; 0 = session cookie.
  expires: number
}

const appSupport = (...p: string[]): string =>
  join(homedir(), 'Library', 'Application Support', ...p)

// ── Chromium family ──────────────────────────────────────────────────────────

interface ChromiumSpec {
  id: BrowserId
  name: string
  baseDir: string
  keychainService: string
  keychainAccount: string
}

const CHROMIUM: ChromiumSpec[] = [
  { id: 'chrome', name: 'Chrome', baseDir: appSupport('Google', 'Chrome'), keychainService: 'Chrome Safe Storage', keychainAccount: 'Chrome' },
  { id: 'edge', name: 'Edge', baseDir: appSupport('Microsoft Edge'), keychainService: 'Microsoft Edge Safe Storage', keychainAccount: 'Microsoft Edge' },
  { id: 'brave', name: 'Brave', baseDir: appSupport('BraveSoftware', 'Brave-Browser'), keychainService: 'Brave Safe Storage', keychainAccount: 'Brave' },
  { id: 'arc', name: 'Arc', baseDir: appSupport('Arc', 'User Data'), keychainService: 'Arc Safe Storage', keychainAccount: 'Arc' },
  { id: 'chromium', name: 'Chromium', baseDir: appSupport('Chromium'), keychainService: 'Chromium Safe Storage', keychainAccount: 'Chromium' },
  { id: 'vivaldi', name: 'Vivaldi', baseDir: appSupport('Vivaldi'), keychainService: 'Vivaldi Safe Storage', keychainAccount: 'Vivaldi' }
]

// Modern Chromium keeps cookies under <profile>/Network/Cookies; older builds
// used <profile>/Cookies. Return whichever exists.
function chromiumCookiesDb(baseDir: string, profile: string): string | null {
  const candidates = [join(baseDir, profile, 'Network', 'Cookies'), join(baseDir, profile, 'Cookies')]
  return candidates.find((p) => existsSync(p)) ?? null
}

// Map profile dir -> display name from Chromium's Local State JSON.
function chromiumDisplayNames(baseDir: string): Record<string, string> {
  const f = join(baseDir, 'Local State')
  if (!existsSync(f)) return {}
  try {
    const json = JSON.parse(readFileSync(f, 'utf8')) as {
      profile?: { info_cache?: Record<string, { name?: string }> }
    }
    const cache = json.profile?.info_cache ?? {}
    const out: Record<string, string> = {}
    for (const [dir, info] of Object.entries(cache)) if (info?.name) out[dir] = info.name
    return out
  } catch {
    return {}
  }
}

function chromiumProfiles(baseDir: string): BrowserProfileInfo[] {
  if (!existsSync(baseDir)) return []
  const names = chromiumDisplayNames(baseDir)
  try {
    return readdirSync(baseDir)
      .filter((dir) => {
        try {
          return statSync(join(baseDir, dir)).isDirectory() && !!chromiumCookiesDb(baseDir, dir)
        } catch {
          return false
        }
      })
      .map((dir) => ({ id: dir, name: names[dir] ?? dir }))
  } catch {
    return []
  }
}

// Cache Safe-Storage keys per service so a multi-profile import prompts once.
const keyCache = new Map<string, Buffer>()

async function chromiumKey(spec: ChromiumSpec): Promise<Buffer> {
  const cached = keyCache.get(spec.keychainService)
  if (cached) return cached
  // Triggers a one-time macOS Keychain access prompt the user must allow.
  const { stdout } = await execFileAsync('security', [
    'find-generic-password',
    '-w',
    '-s',
    spec.keychainService,
    '-a',
    spec.keychainAccount
  ])
  const password = stdout.trim()
  if (!password) throw new Error(`empty "${spec.keychainService}" password`)
  const key = pbkdf2Sync(password, 'saltysalt', 1003, 16, 'sha1')
  keyCache.set(spec.keychainService, key)
  return key
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
    // Strip the SHA256(host) prefix when present (modern Chromium). The hash is
    // 32 random bytes (non-printable); a bare value starts printable.
    if (pt.length >= DOMAIN_HASH_LEN && !isMostlyPrintable(pt.subarray(0, DOMAIN_HASH_LEN))) {
      pt = pt.subarray(DOMAIN_HASH_LEN)
    }
    return pt.toString('utf8')
  } catch {
    return null
  }
}

// Copy a locked SQLite db (+ WAL sidecars) to a private dir and run a query.
async function readSqlite(src: string, query: string): Promise<Record<string, unknown>[]> {
  const tmpDir = mkdtempSync(join(tmpdir(), 'rev-cookies-'))
  try {
    mkdirSync(tmpDir, { recursive: true })
    chmodSync(tmpDir, 0o700)
  } catch {}
  const tmp = join(tmpDir, 'db.sqlite')
  try {
    copyFileSync(src, tmp)
    for (const ext of ['-wal', '-shm']) {
      if (existsSync(src + ext)) copyFileSync(src + ext, tmp + ext)
    }
    const { stdout } = await execFileAsync('sqlite3', ['-json', tmp, query], {
      maxBuffer: 128 * 1024 * 1024
    })
    const trimmed = stdout.trim()
    return trimmed ? (JSON.parse(trimmed) as Record<string, unknown>[]) : []
  } finally {
    rmSync(tmpDir, { force: true, recursive: true })
  }
}

async function readChromium(spec: ChromiumSpec, profile: string): Promise<Cookie[]> {
  const db = chromiumCookiesDb(spec.baseDir, profile)
  if (!db) throw new Error(`no Cookies db for ${spec.name} profile "${profile}"`)
  const key = await chromiumKey(spec)
  const rows = await readSqlite(
    db,
    'SELECT host_key AS host, name, path, is_secure AS secure, is_httponly AS httpOnly, ' +
      'expires_utc AS expires, hex(encrypted_value) AS enc FROM cookies;'
  )
  const out: Cookie[] = []
  for (const r of rows) {
    const value = decryptV10(Buffer.from(String(r.enc), 'hex'), key)
    if (value == null) {
      // Signal undecryptable via a sentinel the caller counts.
      out.push({ host: String(r.host), name: String(r.name), value: UNDECRYPTABLE, path: String(r.path ?? '/'), secure: !!r.secure, httpOnly: !!r.httpOnly, expires: 0 })
      continue
    }
    const micros = Number(r.expires) || 0
    const expires = micros > 0 ? micros / 1e6 - WINDOWS_EPOCH_OFFSET_SECONDS : 0
    out.push({
      host: String(r.host),
      name: String(r.name),
      value,
      path: String(r.path ?? '/') || '/',
      secure: !!r.secure,
      httpOnly: !!r.httpOnly,
      expires
    })
  }
  return out
}

// Sentinel marking a Chromium value we couldn't decrypt (app-bound v20).
const UNDECRYPTABLE = ' __rev_undecryptable__'

// ── Firefox ──────────────────────────────────────────────────────────────────

function firefoxProfilesDir(): string {
  return appSupport('Firefox', 'Profiles')
}

function firefoxProfiles(): BrowserProfileInfo[] {
  const base = firefoxProfilesDir()
  if (!existsSync(base)) return []
  try {
    return readdirSync(base)
      .filter((dir) => {
        try {
          return statSync(join(base, dir)).isDirectory() && existsSync(join(base, dir, 'cookies.sqlite'))
        } catch {
          return false
        }
      })
      // Firefox dirs look like "xxxx.default-release"; show the readable suffix.
      .map((dir) => ({ id: dir, name: dir.includes('.') ? dir.slice(dir.indexOf('.') + 1) : dir }))
  } catch {
    return []
  }
}

async function readFirefox(profile: string): Promise<Cookie[]> {
  const db = join(firefoxProfilesDir(), profile, 'cookies.sqlite')
  if (!existsSync(db)) throw new Error(`no cookies.sqlite for Firefox profile "${profile}"`)
  const rows = await readSqlite(
    db,
    'SELECT host, name, value, path, expiry, isSecure AS secure, isHttpOnly AS httpOnly FROM moz_cookies;'
  )
  return rows.map((r) => ({
    host: String(r.host),
    name: String(r.name),
    value: String(r.value ?? ''),
    path: String(r.path ?? '/') || '/',
    secure: !!r.secure,
    httpOnly: !!r.httpOnly,
    // Firefox stores expiry as unix seconds already.
    expires: Number(r.expiry) || 0
  }))
}

// ── Safari (Cookies.binarycookies) ───────────────────────────────────────────

function safariCookiesFile(): string | null {
  const candidates = [
    join(homedir(), 'Library', 'Containers', 'com.apple.Safari', 'Data', 'Library', 'Cookies', 'Cookies.binarycookies'),
    join(homedir(), 'Library', 'Cookies', 'Cookies.binarycookies')
  ]
  return candidates.find((p) => existsSync(p)) ?? null
}

// Parse Apple's Cookies.binarycookies. Big-endian header/page-count, little-
// endian within each cookie record. See the well-documented layout.
function parseBinaryCookies(buf: Buffer): Cookie[] {
  if (buf.subarray(0, 4).toString('latin1') !== 'cook') throw new Error('not a binarycookies file')
  const numPages = buf.readUInt32BE(4)
  const pageSizes: number[] = []
  let off = 8
  for (let i = 0; i < numPages; i++) {
    pageSizes.push(buf.readUInt32BE(off))
    off += 4
  }
  const cookies: Cookie[] = []
  let pageStart = off
  for (const size of pageSizes) {
    const page = buf.subarray(pageStart, pageStart + size)
    pageStart += size
    const numCookies = page.readUInt32LE(4)
    for (let i = 0; i < numCookies; i++) {
      const cookieOff = page.readUInt32LE(8 + i * 4)
      const c = page.subarray(cookieOff)
      const flags = c.readUInt32LE(8)
      const urlOff = c.readUInt32LE(16)
      const nameOff = c.readUInt32LE(20)
      const pathOff = c.readUInt32LE(24)
      const valueOff = c.readUInt32LE(28)
      const expiry = c.readDoubleLE(40) // seconds since 2001-01-01
      const readStr = (start: number): string => {
        let end = start
        while (end < c.length && c[end] !== 0) end++
        return c.subarray(start, end).toString('utf8')
      }
      const host = readStr(urlOff)
      if (!host) continue
      cookies.push({
        host,
        name: readStr(nameOff),
        value: readStr(valueOff),
        path: readStr(pathOff) || '/',
        secure: (flags & 1) !== 0,
        httpOnly: (flags & 4) !== 0,
        expires: expiry > 0 ? expiry + COCOA_EPOCH_OFFSET_SECONDS : 0
      })
    }
  }
  return cookies
}

function readSafari(): Cookie[] {
  const file = safariCookiesFile()
  if (!file) throw new Error('no Safari Cookies.binarycookies found')
  let buf: Buffer
  try {
    buf = readFileSync(file)
  } catch (e) {
    const code = (e as NodeJS.ErrnoException).code
    if (code === 'EPERM' || code === 'EACCES') {
      throw new Error(
        'Safari cookies are protected. Grant Full Disk Access to Rever Browser in System Settings → Privacy & Security → Full Disk Access, then retry.'
      )
    }
    throw e
  }
  return parseBinaryCookies(buf)
}

// ── Public API ───────────────────────────────────────────────────────────────

// Detected browsers only (installed with at least one importable profile).
export function listBrowsers(): BrowserInfo[] {
  if (process.platform !== 'darwin') return []
  const out: BrowserInfo[] = []
  for (const spec of CHROMIUM) {
    const profiles = chromiumProfiles(spec.baseDir)
    if (profiles.length) out.push({ id: spec.id, name: spec.name, profiles })
  }
  const ff = firefoxProfiles()
  if (ff.length) out.push({ id: 'firefox', name: 'Firefox', profiles: ff })
  if (safariCookiesFile()) {
    out.push({ id: 'safari', name: 'Safari', profiles: [{ id: 'default', name: 'Default' }] })
  }
  return out
}

async function readCookies(opts: ImportOptions): Promise<Cookie[]> {
  const chromium = CHROMIUM.find((s) => s.id === opts.browser)
  if (chromium) return readChromium(chromium, opts.profile || firstChromiumProfile(chromium))
  if (opts.browser === 'firefox') return readFirefox(opts.profile || firefoxProfiles()[0]?.id || '')
  if (opts.browser === 'safari') return readSafari()
  throw new Error(`unknown browser "${opts.browser}"`)
}

function firstChromiumProfile(spec: ChromiumSpec): string {
  const dirs = chromiumProfiles(spec.baseDir).map((p) => p.id)
  return dirs.includes('Default') ? 'Default' : dirs[0] ?? 'Default'
}

export async function importBrowserCookies(opts: ImportOptions): Promise<ImportResult> {
  const empty: ImportResult = { ok: false, imported: 0, skipped: 0, undecryptable: 0, total: 0 }
  if (process.platform !== 'darwin') {
    return { ...empty, error: 'Browser cookie import is macOS-only for now.' }
  }

  const hostFilters = (opts.hosts ?? []).map((h) => h.toLowerCase()).filter(Boolean)
  if (hostFilters.length === 0) {
    console.warn(
      '[browser-cookie-import] WARNING: no host filter — importing ALL cookies. This is a security risk.'
    )
  }

  let all: Cookie[]
  try {
    all = await readCookies(opts)
  } catch (e) {
    return { ...empty, error: e instanceof Error ? e.message : String(e) }
  }

  const matched = hostFilters.length
    ? all.filter((c) => hostFilters.some((f) => c.host.toLowerCase().includes(f)))
    : all

  const sess = session.fromPartition(getActivePartition())
  const nowSec = Date.now() / 1000
  let imported = 0
  let skipped = 0
  let undecryptable = 0

  for (const c of matched) {
    if (c.value === UNDECRYPTABLE) {
      undecryptable++
      continue
    }
    if (c.expires > 0 && c.expires <= nowSec) {
      skipped++ // already expired
      continue
    }
    const isDomainCookie = c.host.startsWith('.')
    const cookieHost = c.host.replace(/^\./, '')
    const url = `${c.secure ? 'https' : 'http'}://${cookieHost}${c.path}`
    try {
      await sess.cookies.set({
        url,
        name: c.name,
        value: c.value,
        path: c.path,
        secure: c.secure,
        httpOnly: c.httpOnly,
        ...(isDomainCookie ? { domain: c.host } : {}),
        ...(c.expires > 0 ? { expirationDate: c.expires } : {})
      })
      imported++
    } catch {
      skipped++
    }
  }

  return { ok: true, imported, skipped, undecryptable, total: matched.length }
}
