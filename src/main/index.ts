import { app, BrowserWindow, ipcMain, Menu, shell, type MenuItemConstructorOptions } from 'electron'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { attachCdpCapture, detachCdpCapture, setActiveTarget } from './chrome-cdp'
import {
  cancelAcpSession,
  killAcpSession,
  promptAcpSession,
  spawnAcpSession,
  type AgentDef
} from './acp-session'
import { getRequest } from './traffic-store'
import { getViewport, setViewport, type ViewportMode } from './viewport'

const __dirname = path.dirname(fileURLToPath(import.meta.url))

// Strip both "Electron/..." and the app-name token (e.g. "rever-browser/0.1.0") from
// the default UA so sites with bot/WAF rules (yes24 Code 12, etc.) don't reject us.
// Result is a clean Chrome UA matching the embedded Chromium version.
app.userAgentFallback = app.userAgentFallback
  .replace(/\s*Electron\/\S+/, '')
  .replace(new RegExp(`\\s*${app.getName()}\\/\\S+`, 'i'), '')

// Bot-detection bypass: prevent Chromium from injecting webdriver=true and
// other AutomationControlled blink features.
app.commandLine.appendSwitch('disable-blink-features', 'AutomationControlled')

let mainWindow: BrowserWindow | null = null

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1600,
    height: 1000,
    backgroundColor: '#0e0e0e',
    titleBarStyle: 'hiddenInset',
    webPreferences: {
      preload: path.join(__dirname, '../preload/index.mjs'),
      contextIsolation: true,
      nodeIntegration: false,
      webviewTag: true,
      sandbox: false
    }
  })

  // Open external links in OS default browser, not inside our window
  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    void shell.openExternal(url)
    return { action: 'deny' }
  })

  // Intercept reload shortcuts so Cmd/Ctrl+R reloads the embedded webview,
  // not the app renderer (which would wipe chat / network state).
  mainWindow.webContents.on('before-input-event', (event, input) => {
    if (input.type !== 'keyDown') return
    const isReload =
      (input.meta || input.control) && input.key.toLowerCase() === 'r' && !input.alt
    if (!isReload) return
    event.preventDefault()
    if (mainWindow && !mainWindow.webContents.isDestroyed()) {
      mainWindow.webContents.send('reload-webview', { ignoreCache: input.shift })
    }
  })

  if (process.env.ELECTRON_RENDERER_URL) {
    void mainWindow.loadURL(process.env.ELECTRON_RENDERER_URL)
  } else {
    void mainWindow.loadFile(path.join(__dirname, '../renderer/index.html'))
  }

  if (!app.isPackaged) {
    mainWindow.webContents.openDevTools({ mode: 'detach' })
  }
}

function installMenu() {
  const isMac = process.platform === 'darwin'
  const sendReload = (ignoreCache: boolean) => {
    if (mainWindow && !mainWindow.webContents.isDestroyed()) {
      mainWindow.webContents.send('reload-webview', { ignoreCache })
    }
  }
  const template: MenuItemConstructorOptions[] = [
    ...(isMac ? ([{ role: 'appMenu' }] as MenuItemConstructorOptions[]) : []),
    { role: 'fileMenu' },
    { role: 'editMenu' },
    {
      label: 'View',
      submenu: [
        {
          label: 'Reload Page',
          accelerator: 'CmdOrCtrl+R',
          click: () => sendReload(false)
        },
        {
          label: 'Hard Reload Page',
          accelerator: 'CmdOrCtrl+Shift+R',
          click: () => sendReload(true)
        },
        { type: 'separator' },
        { role: 'toggleDevTools' },
        { type: 'separator' },
        { role: 'resetZoom' },
        { role: 'zoomIn' },
        { role: 'zoomOut' },
        { type: 'separator' },
        { role: 'togglefullscreen' }
      ]
    },
    { role: 'windowMenu' }
  ]
  Menu.setApplicationMenu(Menu.buildFromTemplate(template))
}

app.whenReady().then(() => {
  installMenu()
  createWindow()

  ipcMain.handle('cdp:attach', async (_event, webContentsId: number) => {
    if (!mainWindow) throw new Error('main window not ready')
    return attachCdpCapture(webContentsId, mainWindow.webContents)
  })

  ipcMain.handle('cdp:detach', async (_event, webContentsId: number) => {
    return detachCdpCapture(webContentsId)
  })

  ipcMain.handle('cdp:set-active', async (_event, webContentsId: number) => {
    return setActiveTarget(webContentsId)
  })

  ipcMain.handle('acp:spawn', async (_event, agentDef: AgentDef, cwd: string) => {
    return spawnAcpSession(agentDef, cwd)
  })

  ipcMain.handle(
    'acp:prompt',
    async (event, sessionId: string, text: string, channel: string) => {
      const sender = event.sender
      return promptAcpSession(sessionId, text, (notification) => {
        if (sender.isDestroyed()) return
        sender.send(channel, notification)
      })
    }
  )

  ipcMain.handle('acp:cancel', async (_event, sessionId: string) => {
    return cancelAcpSession(sessionId)
  })

  ipcMain.handle('acp:kill', async (_event, sessionId: string) => {
    return killAcpSession(sessionId)
  })

  ipcMain.handle('viewport:get', () => getViewport())
  ipcMain.handle('viewport:set', async (_event, mode: ViewportMode) => {
    const next = await setViewport(mode)
    if (mainWindow && !mainWindow.webContents.isDestroyed()) {
      mainWindow.webContents.send('viewport-changed', next)
    }
    return next
  })

  ipcMain.handle('traffic:get', (_event, requestId: string) => {
    return getRequest(requestId) ?? null
  })

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow()
  })
})

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit()
})
