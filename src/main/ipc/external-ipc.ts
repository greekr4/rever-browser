import { ipcMain, type BrowserWindow } from 'electron'

import { launchExternalChrome, killExternalChrome } from '../external-chrome'
import { attachExternalCdp, detachExternalCdp, getExternalTarget } from '../external-cdp'

// External Chrome (Version B) IPC. Extracted from index.ts; needs the main
// window only so screencast frames have a sink. `getMainWindow` is read lazily
// because the window may not exist yet when this registers.
export function registerExternalIpc(getMainWindow: () => BrowserWindow | null): void {
  ipcMain.handle('external:start', async () => {
    const mainWindow = getMainWindow()
    if (!mainWindow) throw new Error('main window not ready')
    console.log('[external] start: launching Chrome…')
    try {
      const { port, pid } = await launchExternalChrome()
      console.log('[external] Chrome launched on port', port, 'pid', pid)
      await attachExternalCdp(port, mainWindow.webContents)
      console.log('[external] CDP attached')
      return { port, pid }
    } catch (e) {
      console.error('[external] start failed:', e)
      throw e
    }
  })

  ipcMain.handle('external:stop', async () => {
    await detachExternalCdp()
    await killExternalChrome()
  })

  ipcMain.handle('external:navigate', async (_event, url: string) => {
    const target = getExternalTarget()
    if (!target) throw new Error('External Chrome not connected')
    await target.navigate(url)
  })

  ipcMain.handle(
    'external:start-screencast',
    async (
      _event,
      opts: { quality?: number; everyNthFrame?: number; maxWidth?: number; maxHeight?: number }
    ) => {
      // Wait up to 10s for external Chrome to be ready (handles race where
      // ScreencastView mounts before external:start completes).
      const deadline = Date.now() + 10_000
      let target = getExternalTarget()
      while (!target && Date.now() < deadline) {
        await new Promise((r) => setTimeout(r, 200))
        target = getExternalTarget()
      }
      if (!target) throw new Error('External Chrome not connected (timed out after 10s)')
      await target.startScreencast(opts)
    }
  )

  ipcMain.handle('external:stop-screencast', async () => {
    const target = getExternalTarget()
    if (target) await target.stopScreencast()
  })

  ipcMain.handle('external:ack-frame', async (_event, sessionId: number) => {
    const target = getExternalTarget()
    if (target) await target.ackScreencast(sessionId)
  })

  ipcMain.handle('external:input-mouse', async (_event, params: unknown) => {
    const target = getExternalTarget()
    if (!target) throw new Error('External Chrome not connected')
    await target.dispatchMouseEvent(params)
  })

  ipcMain.handle('external:input-key', async (_event, params: unknown) => {
    const target = getExternalTarget()
    if (!target) throw new Error('External Chrome not connected')
    await target.dispatchKeyEvent(params)
  })
}
