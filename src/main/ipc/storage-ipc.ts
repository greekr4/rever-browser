import { ipcMain, session } from 'electron'

import { getActiveTarget } from '../chrome-cdp'
import { SHARED_PARTITION } from '../tab-partition'

// Cookie / localStorage / sessionStorage editor IPC, driven by the storage
// panel. Extracted from the index.ts `whenReady` block; it needs nothing from
// there beyond the active CDP target. Register once at startup.
export function registerStorageIpc(): void {
  ipcMain.handle('storage:cookies', async (_event, urls?: string[]) => {
    const target = getActiveTarget()
    if (!target) return { cookies: [], origin: null }
    const origin =
      urls?.[0] ??
      (await (async () => {
        const r = (await target.dbg.sendCommand('Runtime.evaluate', {
          expression: 'location.origin',
          returnByValue: true
        })) as { result: { value?: string } }
        return r.result.value ?? ''
      })())
    const res = (await target.dbg.sendCommand('Network.getCookies', {
      urls: urls?.length ? urls : [origin]
    })) as { cookies: unknown[] }
    return { cookies: res.cookies, origin }
  })

  ipcMain.handle(
    'storage:cookie-set',
    async (
      _event,
      params: {
        name: string
        value: string
        url?: string
        domain?: string
        path?: string
        secure?: boolean
        httpOnly?: boolean
        sameSite?: 'Strict' | 'Lax' | 'None'
        expires?: number
      }
    ) => {
      const target = getActiveTarget()
      if (!target) throw new Error('no active browser target')
      return target.dbg.sendCommand('Network.setCookie', params)
    }
  )

  ipcMain.handle(
    'storage:cookie-delete',
    async (_event, params: { name: string; url?: string; domain?: string; path?: string }) => {
      const target = getActiveTarget()
      if (!target) throw new Error('no active browser target')
      await target.dbg.sendCommand('Network.deleteCookies', params)
      return true
    }
  )

  // Clear every cookie the target host would receive, from the shared partition.
  // A scan reuses the browser's cookie jar; cookies are host- not port-scoped, so
  // a `session`/`ats_token` left by another localhost app is sent to the target
  // and can break it (Flask-Session 500s unpickling a foreign JWT) or skew the
  // auth baseline. Called at scan start for a clean run. Uses the Electron
  // session API (not CDP) so it works before the target tab has attached.
  ipcMain.handle('storage:clear-site-cookies', async (_event, url: string): Promise<number> => {
    const ses = session.fromPartition(SHARED_PARTITION)
    const cookies = await ses.cookies.get({ url }).catch(() => [])
    let cleared = 0
    for (const c of cookies) {
      await ses.cookies.remove(url, c.name).then(
        () => cleared++,
        () => {}
      )
    }
    return cleared
  })

  for (const kind of ['local', 'session'] as const) {
    const storage = `${kind}Storage`
    ipcMain.handle(`storage:${kind}-get`, async () => {
      const target = getActiveTarget()
      if (!target) return {}
      const r = (await target.dbg.sendCommand('Runtime.evaluate', {
        expression: `JSON.stringify(Object.fromEntries(Object.entries(${storage})))`,
        returnByValue: true
      })) as { result: { value?: string } }
      return JSON.parse(r.result.value ?? '{}')
    })
    ipcMain.handle(`storage:${kind}-set`, async (_event, key: string, value: string) => {
      const target = getActiveTarget()
      if (!target) throw new Error('no active browser target')
      await target.dbg.sendCommand('Runtime.evaluate', {
        expression: `${storage}.setItem(${JSON.stringify(key)}, ${JSON.stringify(value)})`
      })
      return true
    })
    ipcMain.handle(`storage:${kind}-delete`, async (_event, key: string) => {
      const target = getActiveTarget()
      if (!target) throw new Error('no active browser target')
      await target.dbg.sendCommand('Runtime.evaluate', {
        expression: `${storage}.removeItem(${JSON.stringify(key)})`
      })
      return true
    })
    ipcMain.handle(`storage:${kind}-clear`, async () => {
      const target = getActiveTarget()
      if (!target) throw new Error('no active browser target')
      await target.dbg.sendCommand('Runtime.evaluate', {
        expression: `${storage}.clear()`
      })
      return true
    })
  }
}
