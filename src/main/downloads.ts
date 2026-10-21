import { app, type Session } from 'electron'
import { existsSync, mkdirSync } from 'node:fs'
import path from 'node:path'

import { emitPageEvent } from './ai-events'

// Downloads from browsed pages. Without a save path Electron opens the macOS
// save dialog, which the agent cannot click, so by default every download is
// saved straight to ~/Downloads/Rever and recorded here for the agent and UI.

export interface DownloadRecord {
  id: number
  url: string
  filename: string
  mime: string
  totalBytes: number
  receivedBytes: number
  state: 'progressing' | 'completed' | 'cancelled' | 'interrupted'
  savePath: string
  startedAt: number
  endedAt?: number
}

const MAX_RECORDS = 100
// wait_for_download ignores downloads that finished longer ago than this.
const RECENT_MS = 30_000

const records: DownloadRecord[] = []
const reported = new Set<number>()
let waiters: Array<() => void> = []
let nextId = 1
let askWhereToSave = false
const attachedSessions = new WeakSet<Session>()

export function getAskWhereToSave(): boolean {
  return askWhereToSave
}

export function setAskWhereToSave(v: boolean): void {
  askWhereToSave = v
}

export function downloadDir(): string {
  return path.join(app.getPath('downloads'), 'Rever')
}

/** `name`, or `name (n)` before the extension, whichever is free. */
export function uniqueFilename(name: string, taken: (n: string) => boolean): string {
  if (!taken(name)) return name
  const ext = path.extname(name)
  const base = name.slice(0, name.length - ext.length)
  for (let i = 1; ; i++) {
    const n = `${base} (${i})${ext}`
    if (!taken(n)) return n
  }
}

export function listDownloads(limit = 50): DownloadRecord[] {
  return records.slice(-limit)
}

export function getDownload(id: number): DownloadRecord | undefined {
  return records.find((r) => r.id === id)
}

/** Downloads started at or after `since`, for tool-result notes. */
export function downloadsStartedSince(since: number): DownloadRecord[] {
  return records.filter((r) => r.startedAt >= since)
}

/**
 * The next finished download not yet returned by this function: one that
 * finished in the last 30s, else the next to finish. Null on timeout.
 */
export async function waitForDownload(timeoutMs: number): Promise<DownloadRecord | null> {
  const deadline = Date.now() + timeoutMs
  for (;;) {
    const done = records.find(
      (r) =>
        r.state !== 'progressing' && !reported.has(r.id) && (r.endedAt ?? 0) >= Date.now() - RECENT_MS
    )
    if (done) {
      reported.add(done.id)
      return done
    }
    const left = deadline - Date.now()
    if (left <= 0) return null
    await new Promise<void>((resolve) => {
      const timer = setTimeout(resolve, left)
      waiters.push(() => {
        clearTimeout(timer)
        resolve()
      })
    })
  }
}

export function attachDownloads(ses: Session): void {
  if (attachedSessions.has(ses)) return
  attachedSessions.add(ses)
  ses.on('will-download', (_event, item) => {
    const rec: DownloadRecord = {
      id: nextId++,
      url: item.getURL(),
      filename: item.getFilename(),
      mime: item.getMimeType(),
      totalBytes: item.getTotalBytes(),
      receivedBytes: 0,
      state: 'progressing',
      savePath: '',
      startedAt: Date.now()
    }
    if (!askWhereToSave) {
      const dir = downloadDir()
      mkdirSync(dir, { recursive: true })
      // Also check in-flight downloads: their files may not exist on disk yet.
      const name = uniqueFilename(item.getFilename(), (n) => {
        const p = path.join(dir, n)
        return existsSync(p) || records.some((r) => r.savePath === p)
      })
      rec.savePath = path.join(dir, name)
      rec.filename = name
      item.setSavePath(rec.savePath)
    }
    records.push(rec)
    if (records.length > MAX_RECORDS) reported.delete(records.shift()!.id)

    item.on('updated', () => {
      rec.receivedBytes = item.getReceivedBytes()
      rec.totalBytes = item.getTotalBytes()
    })
    item.once('done', (_e, state) => {
      rec.state = state
      rec.savePath = item.getSavePath()
      if (rec.savePath) rec.filename = path.basename(rec.savePath)
      rec.receivedBytes = item.getReceivedBytes()
      rec.totalBytes = item.getTotalBytes()
      rec.endedAt = Date.now()
      emitPageEvent({ kind: 'download', download: { ...rec } })
      const ws = waiters
      waiters = []
      for (const w of ws) w()
    })
  })
}
