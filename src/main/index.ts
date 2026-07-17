import { app, BrowserWindow, ipcMain, Menu, nativeTheme, session, shell, type MenuItemConstructorOptions } from 'electron'
import { mkdirSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import type {
  RequestPermissionRequest,
  RequestPermissionResponse
} from '@agentclientprotocol/sdk'

import {
  attachCdpCapture,
  clearDialogHistory,
  detachCdpCapture,
  getActiveTarget,
  getDialogAutoDismiss,
  getDialogHistory,
  setActiveTarget,
  setDialogAutoDismiss
} from './chrome-cdp'
import {
  getSnapshotCount,
  getStickyEnabled,
  initCookiePersistence,
  manualSnapshot,
  setStickyEnabled
} from './cookie-persistence'
import {
  importChromeCookies,
  listChromeProfiles,
  type ChromeImportOptions
} from './chrome-cookie-import'
import { detectAgents, type AgentProbe } from './acp-detect'
import { launchExternalChrome, killExternalChrome } from './external-chrome'
import { attachExternalCdp, detachExternalCdp, getExternalTarget } from './external-cdp'
import {
  cancelSession,
  killSession,
  promptSession,
  sessionModelState,
  setSessionModelRouted,
  spawnSession,
  type AgentDef
} from './agent-router'
import { getApiKey, hasApiKey, setApiKey, type ApiProvider } from './settings'
import {
  getRequest,
  getConsoleSince,
  getExceptions,
  clearConsole,
  clearTraffic,
  listRequests,
  getWsFrames
} from './traffic-store'
import { getViewport, setViewport, type ViewportMode } from './viewport'
import {
  repeaterSend,
  repeaterSendRaw,
  type RepeaterModifications,
  type RepeaterRequestSpec
} from './repeater'
import { partitionForTab, setActivePartition } from './tab-partition'
import { applyTabProxy, proxyCredentialsForSession, type TabProxyConfig } from './tab-proxy'
import { listMcpTools } from './mcp/bridge'
import { cancelWorkflow, runWorkflow, type RunStep } from './workflow-executor'

const __dirname = path.dirname(fileURLToPath(import.meta.url))

// out/main -> project root -> resources/icon.png (Windows/Linux window + dev dock).
const appIcon = path.join(__dirname, '../../resources/icon.png')

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

// ── Agent permission round-trip ───────────────────────────────────────────
// The agent's requestPermission flows main → renderer → main over a
// correlation id. The renderer answers via 'acp:permission-respond'; a guard
// timeout (slightly longer than the renderer's own 60s decision window) rejects
// if the renderer is gone, so the agent loop can't deadlock on a missing UI.
interface PendingPermission {
  resolve: (response: RequestPermissionResponse) => void
  reject: (err: Error) => void
  timer: ReturnType<typeof setTimeout>
}
const pendingPermissions = new Map<string, PendingPermission>()
const PERMISSION_GUARD_MS = 65_000
let permissionSeq = 0

// Window Controls Overlay colors per theme (Windows/Linux). Kept in sync with
// the renderer's app theme via the 'theme:set-titlebar' IPC handler.
const TITLEBAR_OVERLAY = {
  dark: { color: '#161616', symbolColor: '#cfcfcf', height: 40 },
  light: { color: '#eef0f2', symbolColor: '#333333', height: 40 }
} as const

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1600,
    height: 1000,
    backgroundColor: '#0e0e0e',
    // Chrome-like clean chrome: hide the native title bar and let the app's own
    // tab strip act as the draggable title area. On Windows/Linux the native
    // min/max/close buttons render as a Window Controls Overlay in the top-right
    // (color-matched to the app theme); on macOS the traffic lights inset over it.
    // Initial overlay follows the OS scheme; the renderer corrects it to the
    // stored theme on mount via the 'theme:set-titlebar' IPC below.
    ...(process.platform === 'darwin'
      ? { titleBarStyle: 'hiddenInset' as const }
      : {
          titleBarStyle: 'hidden' as const,
          titleBarOverlay: nativeTheme.shouldUseDarkColors
            ? TITLEBAR_OVERLAY.dark
            : TITLEBAR_OVERLAY.light
        }),
    // Packaged builds get the icon from build/icon.{icns,ico}; this covers
    // the Windows/Linux window + taskbar in dev.
    ...(process.platform !== 'darwin' ? { icon: appIcon } : {}),
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

  // macOS dev-mode dock icon (packaged builds use build/icon.icns).
  if (process.platform === 'darwin' && !app.isPackaged) {
    app.dock?.setIcon(appIcon)
  }

  // Real Chrome shows a prompt for accelerometer/camera/clipboard/geolocation/
  // microphone/midi/notifications/etc. — Electron's default is to grant most
  // of them silently, which amiunique flagged at 0.07% similarity. Deny by
  // default so the Permissions API reports 'prompt' / 'denied' like a
  // freshly-installed Chrome with no granted sites.
  //
  // Tabs now each get their own partition (persist:rever-<tabId>), so apply the
  // deny-all handlers to every session as it's created rather than to one fixed
  // partition. The app's own UI runs on the default session — skip it so
  // clipboard/etc. in the renderer keep working.
  app.on('session-created', (createdSession) => {
    if (createdSession === session.defaultSession) return
    createdSession.setPermissionRequestHandler((_wc, _permission, callback) => callback(false))
    createdSession.setPermissionCheckHandler(() => false)
  })

  // Answer per-tab proxy 407 challenges. Site (non-proxy) auth is left to
  // Electron's default (unchanged) by returning without preventDefault.
  app.on('login', (event, webContents, _details, authInfo, callback) => {
    if (!authInfo.isProxy) return
    const creds = proxyCredentialsForSession(webContents.session)
    if (!creds) return
    event.preventDefault()
    callback(creds.username, creds.password)
  })

  // Initialize sticky-session-cookie persistence (no-op if user has it disabled).
  initCookiePersistence()

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

  // ── Per-tab proxy + active-partition tracking ─────────────────────────────
  ipcMain.handle('proxy:set', async (_event, tabId: string, config: TabProxyConfig | null) => {
    await applyTabProxy(tabId, config)
    return true
  })

  // The renderer reports the active tab so cookie-persistence / Chrome import
  // (which use the Electron session.cookies API, not per-webContents CDP) act
  // on the visible tab's partition.
  ipcMain.handle('tab:set-active-partition', (_event, tabId: string) => {
    setActivePartition(partitionForTab(tabId))
    return true
  })

  // ── Workflow executor (macro replay) ──────────────────────────────────────
  ipcMain.handle('workflow:list-tools', () => listMcpTools())
  ipcMain.handle('workflow:run', async (event, steps: RunStep[], channel: string) => {
    const sender = event.sender
    return runWorkflow(steps, (progress) => {
      if (!sender.isDestroyed()) sender.send(channel, progress)
    })
  })
  ipcMain.handle('workflow:cancel', () => {
    cancelWorkflow()
    return true
  })

  ipcMain.handle('theme:set-titlebar', (_event, resolved: 'light' | 'dark') => {
    // macOS has no recolorable overlay (traffic lights); no-op there.
    if (process.platform === 'darwin') return
    if (!mainWindow || mainWindow.isDestroyed()) return
    try {
      mainWindow.setTitleBarOverlay(TITLEBAR_OVERLAY[resolved === 'light' ? 'light' : 'dark'])
    } catch {
      // setTitleBarOverlay throws if the window wasn't created with an overlay
      // (shouldn't happen off-darwin) — ignore.
    }
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
    return spawnSession(agentDef, scratch)
  })

  ipcMain.handle('settings:get-api-key', (_event, provider: ApiProvider) => getApiKey(provider))
  ipcMain.handle('settings:has-api-key', (_event, provider: ApiProvider) => hasApiKey(provider))
  ipcMain.handle('settings:set-api-key', (_event, provider: ApiProvider, key: string) => {
    setApiKey(provider, key)
    return hasApiKey(provider)
  })

  ipcMain.handle(
    'acp:prompt',
    async (event, sessionId: string, text: string, channel: string) => {
      const sender = event.sender
      const requestPermission = (
        req: RequestPermissionRequest
      ): Promise<RequestPermissionResponse> =>
        new Promise((resolve, reject) => {
          if (sender.isDestroyed()) {
            reject(new Error('renderer destroyed'))
            return
          }
          const id = `perm:${permissionSeq++}`
          const timer = setTimeout(() => {
            pendingPermissions.delete(id)
            reject(new Error('permission request timed out (renderer not responding)'))
          }, PERMISSION_GUARD_MS)
          pendingPermissions.set(id, { resolve, reject, timer })
          sender.send('acp:permission-request', { id, request: req })
        })
      return promptSession(
        sessionId,
        text,
        (notification) => {
          if (sender.isDestroyed()) return
          sender.send(channel, notification)
        },
        requestPermission
      )
    }
  )

  ipcMain.on(
    'acp:permission-respond',
    (_event, id: string, response: RequestPermissionResponse) => {
      const pending = pendingPermissions.get(id)
      if (!pending) return
      clearTimeout(pending.timer)
      pendingPermissions.delete(id)
      pending.resolve(response)
    }
  )

  ipcMain.handle('acp:cancel', async (_event, sessionId: string) => {
    return cancelSession(sessionId)
  })

  ipcMain.handle('acp:kill', async (_event, sessionId: string) => {
    return killSession(sessionId)
  })

  ipcMain.handle('acp:model-state', (_event, sessionId: string) => {
    return sessionModelState(sessionId)
  })

  ipcMain.handle('acp:set-model', async (_event, sessionId: string, modelId: string) => {
    return setSessionModelRouted(sessionId, modelId)
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

  ipcMain.handle('traffic:clear', () => {
    clearTraffic()
    return true
  })

  ipcMain.handle(
    'repeater:send',
    async (_event, requestId: string, modifications?: RepeaterModifications) => {
      return repeaterSend(requestId, modifications)
    }
  )
  ipcMain.handle('repeater:send-raw', async (_event, spec: RepeaterRequestSpec) => {
    return repeaterSendRaw(spec)
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

  // ── Storage / Cookie editor IPC ───────────────────────────────────────────
  ipcMain.handle('storage:cookies', async (_event, urls?: string[]) => {
    const target = getActiveTarget()
    if (!target) return { cookies: [], origin: null }
    const origin = urls?.[0] ?? await (async () => {
      const r = (await target.dbg.sendCommand('Runtime.evaluate', {
        expression: 'location.origin',
        returnByValue: true
      })) as { result: { value?: string } }
      return r.result.value ?? ''
    })()
    const res = (await target.dbg.sendCommand('Network.getCookies', {
      urls: urls?.length ? urls : [origin]
    })) as { cookies: unknown[] }
    return { cookies: res.cookies, origin }
  })

  ipcMain.handle('storage:cookie-set', async (_event, params: {
    name: string
    value: string
    url?: string
    domain?: string
    path?: string
    secure?: boolean
    httpOnly?: boolean
    sameSite?: 'Strict' | 'Lax' | 'None'
    expires?: number
  }) => {
    const target = getActiveTarget()
    if (!target) throw new Error('no active browser target')
    return target.dbg.sendCommand('Network.setCookie', params)
  })

  ipcMain.handle('storage:cookie-delete', async (_event, params: {
    name: string
    url?: string
    domain?: string
    path?: string
  }) => {
    const target = getActiveTarget()
    if (!target) throw new Error('no active browser target')
    await target.dbg.sendCommand('Network.deleteCookies', params)
    return true
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

  // ── Sticky session-cookie IPC ─────────────────────────────────────────────
  ipcMain.handle('cookie-persistence:get', () => ({
    enabled: getStickyEnabled(),
    snapshotCount: getSnapshotCount()
  }))
  ipcMain.handle('cookie-persistence:set', (_event, enabled: boolean) => {
    setStickyEnabled(enabled)
    return { enabled }
  })
  ipcMain.handle('cookie-persistence:snapshot', async () => {
    const n = await manualSnapshot()
    return { snapshotCount: n }
  })

  // ── Real Chrome cookie import (macOS) ─────────────────────────────────────
  ipcMain.handle('chrome-cookies:profiles', () => listChromeProfiles())
  ipcMain.handle('chrome-cookies:import', (_event, opts: ChromeImportOptions) =>
    importChromeCookies(opts)
  )

  // ── JS dialog auto-dismiss IPC ────────────────────────────────────────────
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
