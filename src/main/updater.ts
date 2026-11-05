import { app, dialog } from 'electron'
import { autoUpdater } from 'electron-updater'

const CHECK_INTERVAL_MS = 4 * 60 * 60 * 1000
const FIRST_CHECK_DELAY_MS = 30_000

// GitHub-release-based auto-update. In dev (unpackaged) there is no
// app-update.yml, so every check rejects — swallow and stay quiet.
export function initUpdater(): void {
  if (!app.isPackaged) return

  autoUpdater.autoDownload = true
  autoUpdater.logger = console

  const check = (): void => {
    autoUpdater.checkForUpdates().catch((e) => console.warn('[updater] check failed:', e.message))
  }

  setTimeout(check, FIRST_CHECK_DELAY_MS)
  setInterval(check, CHECK_INTERVAL_MS)

  autoUpdater.on('error', (e) => console.warn('[updater] error:', e.message))

  autoUpdater.on('update-downloaded', (info) => {
    void dialog
      .showMessageBox({
        type: 'info',
        message: `Rever Browser ${info.version} has been downloaded`,
        detail: 'Restart now to install the update.',
        buttons: ['Restart Now', 'Later'],
        defaultId: 0
      })
      .then(({ response }) => {
        if (response === 0) autoUpdater.quitAndInstall()
      })
  })
}
