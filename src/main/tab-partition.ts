import { app } from 'electron'
import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

// Browsing sessions. Ordinary tabs live in one persistent Electron partition
// so cookies/storage behave like a normal browser profile — logging in on one
// tab keeps you logged in on tabs opened from it (e.g. Naver cafe posts that
// open in a new tab).
//
// Proxy tabs are the exception: each one gets its own in-memory partition
// (no `persist:` prefix → incognito-style, cookies vanish with the app), so a
// proxied tab browses with a separate cookie jar and its proxy applies to
// that tab alone. The renderer generates the partition name and registers it
// via the proxy:set IPC before navigating the tab.
//
// IMPORTANT: the renderer uses SHARED_PARTITION inline as the webview
// `partition` attribute default in WebviewTab.tsx — keep the two in sync.

export const SHARED_PARTITION = 'persist:rever-shared'

// tabId -> isolated partition. Entries are only added for proxy tabs; every
// other tab resolves to the shared partition. Not cleaned up on tab close —
// entries are a few bytes and tab ids are never reused within a run.
const tabPartitions = new Map<string, string>()

export function registerTabPartition(tabId: string, partition: string): void {
  tabPartitions.set(tabId, partition)
}

export function partitionForTab(tabId: string): string {
  return tabPartitions.get(tabId) ?? SHARED_PARTITION
}

// Features that act on the browsing session via the Electron `session.cookies`
// API (sticky-cookie persistence, Chrome cookie import) resolve the partition
// through these helpers so they target the visible tab's session.
let activePartition = SHARED_PARTITION

export function setActivePartition(partition: string): void {
  activePartition = partition
}

export function getActivePartition(): string {
  return activePartition
}

// ── Browser profiles ─────────────────────────────────────────────────────────
// A profile is a named browsing identity backed by an Electron partition:
//  - "Default" is the shared persistent partition above (implicit, id 'default').
//  - Persistent profiles survive restarts (persist: partition) and are saved to
//    disk. Loading/importing cookies targets one of these.
//  - Incognito profiles use an in-memory partition (cookies vanish with the app)
//    — the same mechanism proxy tabs use, now exposed as an explicit choice.

export type ProfileKind = 'persistent' | 'incognito'

export interface BrowserProfile {
  id: string
  name: string
  kind: ProfileKind
  // Electron partition this profile browses in; derived from id + kind.
  partition: string
  // Source browser this profile was seeded from (e.g. 'Chrome'), for display.
  source?: string
}

export const DEFAULT_PROFILE_ID = 'default'

// id -> {name, kind, source}. The Default profile is implicit and not stored.
const profiles = new Map<string, { name: string; kind: ProfileKind; source?: string }>()

function profilesFile(): string {
  return join(app.getPath('userData'), 'browser-profiles.json')
}

function partitionForProfile(id: string, kind: ProfileKind): string {
  if (id === DEFAULT_PROFILE_ID) return SHARED_PARTITION
  return kind === 'persistent' ? `persist:rever-profile-${id}` : `rever-incognito-${id}`
}

function toProfile(id: string, name: string, kind: ProfileKind, source?: string): BrowserProfile {
  return { id, name, kind, partition: partitionForProfile(id, kind), source }
}

let profilesLoaded = false
function ensureLoaded(): void {
  if (profilesLoaded) return
  profilesLoaded = true
  try {
    const file = profilesFile()
    if (!existsSync(file)) return
    const raw = JSON.parse(readFileSync(file, 'utf8')) as Array<{
      id: string
      name: string
      source?: string
    }>
    for (const p of raw) profiles.set(p.id, { name: p.name, kind: 'persistent', source: p.source })
  } catch (e) {
    console.error('[profiles] load failed', e)
  }
}

function persistProfiles(): void {
  // Only persistent profiles are saved; incognito ones are ephemeral by design.
  const out: Array<{ id: string; name: string; source?: string }> = []
  for (const [id, p] of profiles)
    if (p.kind === 'persistent') out.push({ id, name: p.name, source: p.source })
  try {
    writeFileSync(profilesFile(), JSON.stringify(out, null, 2))
  } catch (e) {
    console.error('[profiles] save failed', e)
  }
}

export function listProfiles(): BrowserProfile[] {
  ensureLoaded()
  const list = [toProfile(DEFAULT_PROFILE_ID, 'Default', 'persistent')]
  for (const [id, p] of profiles) list.push(toProfile(id, p.name, p.kind, p.source))
  return list
}

export function createProfile(name: string, kind: ProfileKind, source?: string): BrowserProfile {
  ensureLoaded()
  const id = `p${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`
  const trimmed = (name ?? '').trim() || (kind === 'incognito' ? 'Incognito' : 'Profile')
  profiles.set(id, { name: trimmed, kind, source })
  if (kind === 'persistent') persistProfiles()
  return toProfile(id, trimmed, kind, source)
}

export function deleteProfile(id: string): void {
  if (id === DEFAULT_PROFILE_ID) return
  ensureLoaded()
  const p = profiles.get(id)
  profiles.delete(id)
  if (p?.kind === 'persistent') persistProfiles()
}
