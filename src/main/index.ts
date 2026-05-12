import { app, BrowserWindow, ipcMain, Menu, session, shell, type MenuItemConstructorOptions } from 'electron'
import { mkdirSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { attachCdpCapture, detachCdpCapture, setActiveTarget } from './chrome-cdp'
import { detectAgents, type AgentProbe } from './acp-detect'
import { launchExternalChrome, killExternalChrome } from './external-chrome'
import { attachExternalCdp, detachExternalCdp, getExternalTarget } from './external-cdp'
import {
  cancelAcpSession,
  getSessionModelState,
  killAcpSession,
  promptAcpSession,
  setSessionModel,
  spawnAcpSession,
  type AgentDef
} from './acp-session'
import {
  getRequest,
  getConsoleSince,
  getExceptions,
  clearConsole,
  listRequests,
  getWsFrames
} from './traffic-store'
import { getViewport, setViewport, type ViewportMode } from './viewport'

const __dirname = path.dirname(fileURLToPath(import.meta.url))

// Strip "Electron/..." and the app-name token from the default UA so sites
// with bot/WAF rules don't reject us. The embedded Chrome version is left
// untouched — Electron 41 ships Chromium 146, which matches the engine's
// internal behaviour and survives Pixelscan's "legitimate" check.
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

  // Real Chrome shows a prompt for accelerometer/camera/clipboard/geolocation/
  // microphone/midi/notifications/etc. — Electron's default is to grant most
  // of them silently, which amiunique flagged at 0.07% similarity. Deny by
  // default so the Permissions API reports 'prompt' / 'denied' like a
  // freshly-installed Chrome with no granted sites.
  const revSession = session.fromPartition('persist:rever')
  revSession.setPermissionRequestHandler((_wc, _permission, callback) => callback(false))
  revSession.setPermissionCheckHandler(() => false)

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

  ipcMain.handle('acp:list-available', async (_event, probes: AgentProbe[]) => {
    return detectAgents(probes)
  })

  ipcMain.handle('acp:spawn', async (_event, agentDef: AgentDef, _cwd: string) => {
    // Always sandbox the agent in a scratch directory under userData so
    // Edit/Write/Bash tools cannot accidentally mutate the rever-browser
    // source tree. The renderer's cwd hint is intentionally ignored.
    const scratch = path.join(app.getPath('userData'), 'agent-scratch')
    try {
      mkdirSync(scratch, { recursive: true })
    } catch (e) {
      console.warn('[acp:spawn] failed to ensure scratch dir', e)
    }
    return spawnAcpSession(agentDef, scratch)
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

  ipcMain.handle('acp:model-state', (_event, sessionId: string) => {
    return getSessionModelState(sessionId)
  })

  ipcMain.handle('acp:set-model', async (_event, sessionId: string, modelId: string) => {
    return setSessionModel(sessionId, modelId)
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

  ipcMain.handle('console:list', (_event, since?: number, limit?: number) =>
    getConsoleSince(since ?? 0).slice(-(limit ?? 200))
  )
  ipcMain.handle('console:exceptions', (_event, limit?: number) =>
    getExceptions().slice(-(limit ?? 100))
  )
  ipcMain.handle('console:clear', () => clearConsole())
  ipcMain.handle('ws:list', () =>
    listRequests({ limit: 200 }).filter((r) => r.resourceType === 'WebSocket')
  )
  ipcMain.handle('ws:frames', (_event, requestId: string, since?: number, limit?: number) =>
    getWsFrames(requestId, since).slice(-(limit ?? 100))
  )

  // ── External Chrome (Version B) IPC ────────────────────────────────────────

  ipcMain.handle('external:start', async () => {
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

  ipcMain.handle('external:start-screencast', async (_event, opts: {
    quality?: number
    everyNthFrame?: number
    maxWidth?: number
    maxHeight?: number
  }) => {
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
  })

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

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow()
  })
})

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit()
})
