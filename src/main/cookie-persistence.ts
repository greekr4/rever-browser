import { app, session, type Cookie } from 'electron'
import { readFileSync, writeFileSync, existsSync } from 'node:fs'
import path from 'node:path'

const PARTITION = 'persist:rever'
const SETTINGS_FILE = 'sticky-cookies-settings.json'
const STORE_FILE = 'sticky-session-cookies.json'

// Extend session-cookie lifetime by this many seconds when we re-inject them.
// 30 days — long enough to survive day-to-day reversing sessions, short
// enough that they don't accumulate forever.
const STICKY_EXTEND_SECONDS = 60 * 60 * 24 * 30

interface SettingsFile {
  stickyEnabled: boolean
}

interface StickyEntry {
  url: string
  name: string
  value: string
  domain?: string
  path?: string
  secure?: boolean
  httpOnly?: boolean
  sameSite?: 'unspecified' | 'no_restriction' | 'lax' | 'strict'
}

let settings: SettingsFile = { stickyEnabled: false }

function settingsPath(): string {
  return path.join(app.getPath('userData'), SETTINGS_FILE)
}
function storePath(): string {
  return path.join(app.getPath('userData'), STORE_FILE)
}

function loadSettings(): void {
  try {
    if (existsSync(settingsPath())) {
      settings = JSON.parse(readFileSync(settingsPath(), 'utf8'))
    }
  } catch (e) {
    console.error('[sticky-cookies] settings read failed:', e)
  }
}

function saveSettings(): void {
  try {
    writeFileSync(settingsPath(), JSON.stringify(settings, null, 2))
  } catch (e) {
    console.error('[sticky-cookies] settings write failed:', e)
  }
}

export function getStickyEnabled(): boolean {
  return settings.stickyEnabled
}

export function setStickyEnabled(v: boolean): void {
  settings.stickyEnabled = v
  saveSettings()
}

function cookieToUrl(c: Cookie): string {
  const protocol = c.secure ? 'https' : 'http'
  // Strip leading dot from domain; .example.com is the Set-Cookie style but URLs need example.com.
  const host = (c.domain ?? '').replace(/^\./, '')
  return `${protocol}://${host}${c.path ?? '/'}`
}

function sameSiteOut(s: Cookie['sameSite']): StickyEntry['sameSite'] {
  switch (s) {
    case 'lax':
    case 'strict':
    case 'no_restriction':
    case 'unspecified':
      return s
    default:
      return undefined
  }
}

async function dumpSessionCookies(): Promise<void> {
  if (!settings.stickyEnabled) return
  try {
    const sess = session.fromPartition(PARTITION)
    const all = await sess.cookies.get({})
    const sessionOnly = all.filter((c) => c.session === true)
    const entries: StickyEntry[] = sessionOnly.map((c) => ({
      url: cookieToUrl(c),
      name: c.name,
      value: c.value,
      domain: c.domain,
      path: c.path,
      secure: c.secure,
      httpOnly: c.httpOnly,
      sameSite: sameSiteOut(c.sameSite)
    }))
    writeFileSync(storePath(), JSON.stringify(entries, null, 2))
    console.log(`[sticky-cookies] dumped ${entries.length} session cookies to disk`)
  } catch (e) {
    console.error('[sticky-cookies] dump failed:', e)
  }
}

async function restoreSessionCookies(): Promise<void> {
  if (!settings.stickyEnabled) return
  try {
    if (!existsSync(storePath())) return
    const raw = readFileSync(storePath(), 'utf8')
    const entries: StickyEntry[] = JSON.parse(raw)
    const sess = session.fromPartition(PARTITION)
    const expiry = Math.floor(Date.now() / 1000) + STICKY_EXTEND_SECONDS
    let restored = 0
    for (const e of entries) {
      try {
        await sess.cookies.set({
          url: e.url,
          name: e.name,
          value: e.value,
          domain: e.domain,
          path: e.path,
          secure: e.secure,
          httpOnly: e.httpOnly,
          sameSite: e.sameSite,
          expirationDate: expiry
        })
        restored++
      } catch (err) {
        // Per-cookie set failures (invalid domain, etc.) shouldn't kill the restore.
        console.warn('[sticky-cookies] restore single failed:', e.name, err)
      }
    }
    console.log(`[sticky-cookies] restored ${restored}/${entries.length} session cookies`)
  } catch (e) {
    console.error('[sticky-cookies] restore failed:', e)
  }
}

export function initCookiePersistence(): void {
  loadSettings()
  // Restore before any webview navigates. Caller invokes this from
  // app.whenReady() before createWindow().
  void restoreSessionCookies()
  // Hook quit so we can dump session cookies before the process dies.
  app.on('before-quit', async (e) => {
    if (!settings.stickyEnabled) return
    // Defer quit a tick so the async dump completes.
    e.preventDefault()
    await dumpSessionCookies()
    app.exit(0)
  })
}

export async function manualSnapshot(): Promise<number> {
  // Allow on-demand dump from the UI (without waiting for quit).
  const sess = session.fromPartition(PARTITION)
  const all = await sess.cookies.get({})
  const sessionOnly = all.filter((c) => c.session === true)
  const entries: StickyEntry[] = sessionOnly.map((c) => ({
    url: cookieToUrl(c),
    name: c.name,
    value: c.value,
    domain: c.domain,
    path: c.path,
    secure: c.secure,
    httpOnly: c.httpOnly,
    sameSite: sameSiteOut(c.sameSite)
  }))
  writeFileSync(storePath(), JSON.stringify(entries, null, 2))
  return entries.length
}

export function getSnapshotCount(): number {
  try {
    if (!existsSync(storePath())) return 0
    const raw = readFileSync(storePath(), 'utf8')
    return JSON.parse(raw).length
  } catch {
    return 0
  }
}
