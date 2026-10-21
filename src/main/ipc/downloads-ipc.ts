import { ipcMain, shell } from 'electron'

import { getAskWhereToSave, getDownload, listDownloads, setAskWhereToSave } from '../downloads'

export function registerDownloadsIpc(): void {
  ipcMain.handle('downloads:list', (_event, limit?: number) => listDownloads(limit ?? 50))
  ipcMain.handle('downloads:get-settings', () => ({ askWhereToSave: getAskWhereToSave() }))
  ipcMain.handle('downloads:set-ask', (_event, enabled: boolean) => {
    setAskWhereToSave(enabled)
    return { askWhereToSave: getAskWhereToSave() }
  })
  // Takes an id, not a path, so the renderer can only reveal files we saved.
  ipcMain.handle('downloads:reveal', (_event, id: number) => {
    const d = getDownload(id)
    if (d?.state !== 'completed' || !d.savePath) return false
    shell.showItemInFolder(d.savePath)
    return true
  })
}
