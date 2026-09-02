import { ipcMain } from 'electron'

import { getActiveTarget } from '../chrome-cdp'

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
