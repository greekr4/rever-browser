import { ipcMain } from 'electron'

import {
  getDialogAutoDismiss,
  setDialogAutoDismiss,
  getDialogHistory,
  clearDialogHistory
} from '../chrome-cdp'

// JS dialog auto-dismiss IPC. Extracted from index.ts.
export function registerDialogIpc(): void {
  ipcMain.handle('dialog:get-settings', () => ({
    autoDismiss: getDialogAutoDismiss(),
    history: getDialogHistory(50)
  }))
  ipcMain.handle('dialog:set-auto-dismiss', (_event, enabled: boolean) => {
    setDialogAutoDismiss(enabled)
    return { autoDismiss: enabled }
  })
  ipcMain.handle('dialog:history', (_event, limit?: number) => getDialogHistory(limit ?? 50))
  ipcMain.handle('dialog:clear-history', () => {
    clearDialogHistory()
    return true
  })
}
