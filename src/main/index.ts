import { app, BrowserWindow, clipboard, ipcMain, Menu, nativeTheme, net, session, shell, type MenuItemConstructorOptions } from 'electron'
import { mkdirSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import type {
  RequestPermissionRequest,
  RequestPermissionResponse
} from '@agentclientprotocol/sdk'

import { startMcpServer } from './mcp/server'
import { refForBackendNodeId } from './mcp/snapshot'
import {
  attachCdpCapture,
  clearDialogHistory,
  detachCdpCapture,
  getActiveTarget,
  getDialogAutoDismiss,
  getDialogHistory,
  setActiveTarget,
  setDialogAutoDismiss,
  setEmulatedColorScheme,
  setInspectNodeHandler
} from './chrome-cdp'
import {
  getSnapshotCount,
  getStickyEnabled,
  initCookiePersistence,
  manualSnapshot,
  setStickyEnabled
} from './cookie-persistence'
import {
  importBrowserCookies,
  listBrowsers,
  type ImportOptions
} from './browser-cookie-import'
import { detectAgents, type AgentProbe } from './acp-detect'
import { skillInstalled } from './skill-install'
import {
  spawnTerminal,
  writeTerminal,
  resizeTerminal,
  killTerminal,
  killAllTerminals
} from './terminal'
import { initRendererBridge } from './renderer-bridge'
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
import {
  getActivePartition,
  partitionForTab,
  registerTabPartition,
  setActivePartition,
  listProfiles,
  createProfile,
  deleteProfile,
  type ProfileKind
} from './tab-partition'
import { applyTabProxy, proxyCredentialsForSession, type TabProxyConfig } from './tab-proxy'
import { listMcpTools } from './mcp/bridge'
import {
  cancelWorkflow,
  runPipeline,
  runWorkflow,
  type ResolvedPipeNode,
  type RunStep
} from './workflow-executor'

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

// Display name for menus, About panel and notifications. Must come after the
// UA strip above (which matches the original package name) and before
// installMenu (whose appMenu role reads app.name). userData is pinned to the
// original 'rever-browser' directory so mcp-endpoint.json, cookies and
// localStorage keep their existing location.
app.setName('Rever Browser')
app.setPath('userData', path.join(app.getPath('appData'), 'rever-browser'))
app.setAboutPanelOptions({
  applicationName: 'Rever Browser',
  applicationVersion: app.getVersion()
})

// Bot-detection bypass: prevent Chromium from injecting webdriver=true and
// other AutomationControlled blink features.
app.commandLine.appendSwitch('disable-blink-features', 'AutomationControlled')

// Encrypt the persisted cookie store with a built-in key instead of an OS
// keychain entry. This app ships unsigned/ad-hoc, so a keychain-backed key is
// tied to the code signature: every update changes the signature, which re-fires
// the macOS keychain prompt ("Rever Browser Safe Storage") and makes previously
// saved cookies undecryptable — silently logging users out on each release.
// macOS ignores --password-store (Linux-only); the mac lever is
// --use-mock-keychain, which routes os_crypt/safeStorage through a stable
// in-process key with no Keychain access and no prompt. Cookies still persist
// across launches (weaker at-rest crypto is acceptable for a local reversing
// tool). password-store=basic covers the same case on Linux.
app.commandLine.appendSwitch('use-mock-keychain')
app.commandLine.appendSwitch('password-store', 'basic')

// Keep the renderer running at full speed when the window is not the frontmost
// one. Chromium stops producing compositor frames for a hidden or occluded
// window, and CDP input dispatch is acked off that pipeline: with the window
// merely behind another app, each Input.dispatchMouseEvent took ~5s to return,
// turning one browser_hover into 70s. An agent driving the browser without
// watching it is the normal case here, so backgrounded must cost nothing.
app.commandLine.appendSwitch('disable-backgrounding-occluded-windows')
app.commandLine.appendSwitch('disable-renderer-backgrounding')
app.commandLine.appendSwitch('disable-background-timer-throttling')

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
      sandbox: false,
      // See the command-line switches above: a backgrounded window must still
      // render, or CDP input dispatch stalls for seconds per event.
      backgroundThrottling: false
    }
  })

  // Open external links in OS default browser, not inside our window
  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    void shell.openExternal(url)
    return { action: 'deny' }
  })

  // Plain link clicks in the app UI (e.g. chat markdown) navigate the renderer
  // itself, wiping chat / network state. Block them and open in an in-app tab.
  // Navigations to the app's own URL (dev-server HMR full reload) stay allowed.
  const appUrl = process.env.ELECTRON_RENDERER_URL ?? 'file://'
  mainWindow.webContents.on('will-navigate', (event, url) => {
    if (url.startsWith(appUrl)) return
    event.preventDefault()
    if (/^https?:/i.test(url)) sendBrowserCommand('open-tab', { url })
  })

  // Intercept browser shortcuts at the keyboard level. Menu accelerators cover
  // the discoverable items; this covers Cmd/Ctrl+R (reload the webview, not the
  // app renderer — that would wipe chat / network state), Cmd/Ctrl+L and
  // Cmd/Ctrl+1..9. The same handler is attached to webview contents in
  // whenReady so shortcuts work while the page has focus too.
  mainWindow.webContents.on('before-input-event', (event, input) => {
    if (handleBrowserShortcut(input)) event.preventDefault()
  })

  // Right-click in the app UI: text-editing menu only. No Back/Reload here —
  // reloading the app renderer would wipe chat / network state.
  mainWindow.webContents.on('context-menu', (event, params) => {
    if (!mainWindow) return
    if (!params.isEditable && !params.selectionText.trim()) return
    event.preventDefault()
    const contents = mainWindow.webContents
    const items: MenuItemConstructorOptions[] = params.isEditable
      ? [
          { label: 'Cut', enabled: params.editFlags.canCut, click: () => contents.cut() },
          { label: 'Copy', enabled: params.editFlags.canCopy, click: () => contents.copy() },
          { label: 'Paste', enabled: params.editFlags.canPaste, click: () => contents.paste() },
          { label: 'Select All', click: () => contents.selectAll() }
        ]
      : [{ label: 'Copy', click: () => contents.copy() }]
    Menu.buildFromTemplate(items).popup({ window: mainWindow })
  })

  if (process.env.ELECTRON_RENDERER_URL) {
    void mainWindow.loadURL(process.env.ELECTRON_RENDERER_URL)
  } else {
    void mainWindow.loadFile(path.join(__dirname, '../renderer/index.html'))
  }

  // DevTools no longer opens by itself in dev — it stole focus and covered the
  // window on every restart. View › Toggle Developer Tools still opens it.
}

// Forward a browser-level command (new tab, close tab, tab switching, ...) to
// the renderer, which owns the tab store.
function sendBrowserCommand(cmd: string, extra?: { index?: number; url?: string }): void {
  if (mainWindow && !mainWindow.webContents.isDestroyed()) {
    mainWindow.webContents.send('browser-command', { cmd, ...extra })
  }
}

// Chromium-style right-click menu for the browsed page (webview contents).
function buildPageContextMenu(
  contents: Electron.WebContents,
  params: Electron.ContextMenuParams
): Menu {
  const items: MenuItemConstructorOptions[] = []
  if (params.linkURL) {
    items.push(
      {
        label: 'Open Link in New Tab',
        click: () => sendBrowserCommand('open-tab', { url: params.linkURL })
      },
      { label: 'Copy Link Address', click: () => clipboard.writeText(params.linkURL) },
      { type: 'separator' }
    )
  }
  if (params.mediaType === 'image' && params.srcURL) {
    items.push(
      {
        label: 'Open Image in New Tab',
        click: () => sendBrowserCommand('open-tab', { url: params.srcURL })
      },
      { label: 'Copy Image Address', click: () => clipboard.writeText(params.srcURL) },
      { label: 'Save Image As…', click: () => contents.downloadURL(params.srcURL) },
      { type: 'separator' }
    )
  }
  if (params.isEditable) {
    items.push(
      { label: 'Cut', enabled: params.editFlags.canCut, click: () => contents.cut() },
      { label: 'Copy', enabled: params.editFlags.canCopy, click: () => contents.copy() },
      { label: 'Paste', enabled: params.editFlags.canPaste, click: () => contents.paste() },
      { label: 'Select All', click: () => contents.selectAll() },
      { type: 'separator' }
    )
  } else if (params.selectionText.trim()) {
    items.push({ label: 'Copy', click: () => contents.copy() }, { type: 'separator' })
  }
  if (!params.linkURL && !params.isEditable && params.mediaType === 'none') {
    items.push(
      {
        label: 'Back',
        enabled: contents.navigationHistory.canGoBack(),
        click: () => contents.navigationHistory.goBack()
      },
      {
        label: 'Forward',
        enabled: contents.navigationHistory.canGoForward(),
        click: () => contents.navigationHistory.goForward()
      },
      { label: 'Reload', click: () => contents.reload() },
      { type: 'separator' }
    )
  }
  items.push(
    {
      label: 'Copy Element',
      click: () => void copyElementInfo(contents, params.x, params.y)
    },
    {
      label: 'Inspect Element',
      click: () => contents.inspectElement(params.x, params.y)
    }
  )
  return Menu.buildFromTemplate(items)
}

// Selector builder shared by "Copy Element" and the element picker: walk up
// from the element (`this`) preferring a unique #id, then a stable attribute,
// then tag:nth-of-type — stopping at an id-anchored ancestor or body.
// Self-contained; runs in the page. The context menu calls it via
// executeJavaScript on elementFromPoint's result; the picker via
// Runtime.callFunctionOn with the picked node as `this` — both paths produce
// identical selectors because this is the only copy of the walk.
const SELECTOR_FROM_ELEMENT_FN = `function () {
  const el = this;
  if (!el || el.nodeType !== 1) return null;
  const doc = el.ownerDocument;
  const parts = [];
  let node = el;
  while (node && node.nodeType === 1 && node !== doc.documentElement) {
    if (node.id && doc.querySelectorAll('#' + CSS.escape(node.id)).length === 1) {
      parts.unshift('#' + CSS.escape(node.id));
      return parts.join(' > ');
    }
    if (node.tagName === 'BODY') {
      parts.unshift('body');
      return parts.join(' > ');
    }
    let seg = null;
    for (const attr of ['data-testid', 'data-test', 'name', 'aria-label']) {
      const v = node.getAttribute(attr);
      if (v) {
        seg = node.tagName.toLowerCase() + '[' + attr + '=' + JSON.stringify(v) + ']';
        break;
      }
    }
    if (seg && doc.querySelectorAll(seg).length === 1) {
      parts.unshift(seg);
      return parts.join(' > ');
    }
    if (!seg) {
      let k = 1;
      let s = node;
      while ((s = s.previousElementSibling)) if (s.tagName === node.tagName) k++;
      seg = node.tagName.toLowerCase() + ':nth-of-type(' + k + ')';
    }
    parts.unshift(seg);
    node = node.parentElement;
  }
  return parts.length ? parts.join(' > ') : null;
}`

interface ElementRect {
  x: number
  y: number
  width: number
  height: number
}

// Bounding rect (webview-viewport CSS px) of a CDP box-model content quad
// [x1,y1,x2,y2,x3,y3,x4,y4].
function contentQuadRect(q: number[] | undefined): ElementRect | null {
  if (!q || q.length < 8) return null
  const xs = [q[0], q[2], q[4], q[6]]
  const ys = [q[1], q[3], q[5], q[7]]
  const x = Math.min(...xs)
  const y = Math.min(...ys)
  return {
    x: Math.round(x),
    y: Math.round(y),
    width: Math.round(Math.max(...xs) - x),
    height: Math.round(Math.max(...ys) - y)
  }
}

// Shared tail of both copy paths: write the clipboard text and ask the UI to
// outline the element with a "Copied!" badge (rect = webview-viewport CSS px).
function publishElementCopy(
  rect: ElementRect | null,
  selector: string | null,
  ref: string | null
): void {
  clipboard.writeText(
    `selector: ${selector ?? '(none — no element at this point)'}\n` +
      `ref: ${ref ? `${ref}   (current snapshot only)` : '(none — run a browser_snapshot first)'}`
  )
  mainWindow?.webContents.send('element-copied', { rect, selector, ref })
  // Any element copy (picker click OR the right-click menu) exits picker mode.
  if (pickerActive) void stopPicker()
}

// "Copy Element": put a robust CSS selector for the element under the cursor on
// the clipboard, plus its current snapshot ref (rN) when one exists. Both
// lookups are best-effort — a failed one degrades to a "(none — ...)" line.
async function copyElementInfo(
  contents: Electron.WebContents,
  x: number,
  y: number
): Promise<void> {
  let selector: string | null = null
  try {
    selector = (await contents.executeJavaScript(
      `(${SELECTOR_FROM_ELEMENT_FN}).call(document.elementFromPoint(${x}, ${y}))`
    )) as string | null
  } catch {
    // Page navigated away or script execution blocked — selector stays null.
  }

  // Ref + rect: resolve the point to a backendNodeId via CDP, reverse-look it up
  // in the current snapshot's ref map (page session — sessionId undefined), and
  // grab its box model so the UI can outline the element.
  let ref: string | null = null
  let rect: ElementRect | null = null
  try {
    await contents.debugger.sendCommand('DOM.enable')
    const { backendNodeId } = (await contents.debugger.sendCommand('DOM.getNodeForLocation', {
      x,
      y,
      includeUserAgentShadowDOM: false
    })) as { backendNodeId?: number }
    if (backendNodeId != null) {
      ref = refForBackendNodeId(backendNodeId)
      try {
        const { model } = (await contents.debugger.sendCommand('DOM.getBoxModel', {
          backendNodeId
        })) as { model: { content: number[] } }
        rect = contentQuadRect(model.content)
      } catch {
        // Node has no box model (hidden) — rect stays null.
      }
    }
  } catch {
    // Debugger not attached / CDP not ready — ref/rect stay null.
  }

  publishElementCopy(rect, selector, ref)
}

// ---- Element picker (DevTools-style inspect mode) ---------------------------
// startPicker turns on CDP Overlay inspect mode on the active webview: hovering
// highlights elements with the native DevTools overlay (box model + tag +
// size); clicking fires Overlay.inspectNodeRequested (routed here from
// chrome-cdp.ts's message listener), which copies the element's selector + ref
// exactly like "Copy Element" and exits the mode.

// picker and grab share one CDP inspect overlay but are distinct modes. Only
// one is ever active; `grabMode` selects what a click does. The renderer gets
// both bits via picker:state so its two toolbar toggles never fight.
let pickerActive = false
let grabMode = false

function sendPickerState(): void {
  if (mainWindow && !mainWindow.webContents.isDestroyed()) {
    const mode = pickerActive ? (grabMode ? 'grab' : 'pick') : null
    mainWindow.webContents.send('picker:state', { active: pickerActive, mode })
  }
}

async function enableInspect(): Promise<boolean> {
  const target = getActiveTarget()
  if (!target) return false
  try {
    await target.dbg.sendCommand('DOM.enable')
    await target.dbg.sendCommand('Overlay.enable')
    await target.dbg.sendCommand('Overlay.setInspectMode', {
      mode: 'searchForNode',
      highlightConfig: {
        showInfo: true,
        showStyles: false,
        contentColor: { r: 111, g: 168, b: 220, a: 0.4 },
        paddingColor: { r: 147, g: 196, b: 125, a: 0.4 },
        borderColor: { r: 255, g: 229, b: 153, a: 0.4 },
        marginColor: { r: 246, g: 178, b: 107, a: 0.4 }
      }
    })
    return true
  } catch (e) {
    // Debugger detached mid-call — stay off rather than crash.
    console.error('[inspect] enable failed:', e)
    return false
  }
}

async function disableInspect(): Promise<void> {
  const target = getActiveTarget()
  if (target) {
    try {
      await target.dbg.sendCommand('Overlay.setInspectMode', { mode: 'none' })
      await target.dbg.sendCommand('Overlay.disable')
    } catch {
      // Debugger detached or page gone — the overlay died with it.
    }
  }
}

async function startPicker(): Promise<void> {
  grabMode = false
  pickerActive = await enableInspect()
  sendPickerState()
}

// Shared stop for both modes: kills the overlay and clears both bits.
async function stopPicker(): Promise<void> {
  await disableInspect()
  pickerActive = false
  grabMode = false
  sendPickerState()
}

// The user clicked an element while the picker was active: resolve it to a
// selector (same builder as "Copy Element") and snapshot ref, flash the copy
// animation at the element's center, then exit picker mode.
async function onPickerNodeRequested(backendNodeId: number): Promise<void> {
  const target = getActiveTarget()
  if (!target) {
    await stopPicker()
    return
  }

  let selector: string | null = null
  try {
    const { object } = (await target.dbg.sendCommand('DOM.resolveNode', { backendNodeId })) as {
      object: { objectId?: string }
    }
    if (object.objectId) {
      const res = (await target.dbg.sendCommand('Runtime.callFunctionOn', {
        objectId: object.objectId,
        functionDeclaration: SELECTOR_FROM_ELEMENT_FN,
        returnByValue: true
      })) as { result: { value?: unknown } }
      if (typeof res.result?.value === 'string') selector = res.result.value
    }
  } catch {
    // Node vanished or debugger detached — selector stays null.
  }

  const ref = refForBackendNodeId(backendNodeId)

  // Element rect (webview-viewport CSS px) so the UI can outline what was picked.
  let rect: ElementRect | null = null
  try {
    const { model } = (await target.dbg.sendCommand('DOM.getBoxModel', { backendNodeId })) as {
      model: { content: number[] }
    }
    rect = contentQuadRect(model.content)
  } catch {
    // No box model (node hidden mid-flight) — rect stays null.
  }

  publishElementCopy(rect, selector, ref)
  await stopPicker()
}

// ---- Grab mode (element → screenshot + context for the agent) ---------------
// Reuses the same CDP inspect overlay as the picker, but on click it captures
// an element screenshot and its context and hands them to the renderer (chat
// context + markup editor) instead of copying a selector to the clipboard.

const GRAB_ELEMENT_INFO_FN = `function () {
  return {
    tag: this.tagName ? this.tagName.toLowerCase() : null,
    text: ((this.innerText || this.textContent || '').trim()).slice(0, 200),
    id: this.id || null,
    cls: typeof this.className === 'string' ? this.className : null
  }
}`

async function startGrab(): Promise<void> {
  grabMode = true
  pickerActive = await enableInspect()
  if (!pickerActive) grabMode = false
  sendPickerState()
}

// stopGrab is just the shared stop (kept as a named alias for call-site clarity).
async function stopGrab(): Promise<void> {
  await stopPicker()
}

async function onGrabNodeRequested(backendNodeId: number): Promise<void> {
  const target = getActiveTarget()
  if (!target) {
    await stopGrab()
    return
  }

  let selector: string | null = null
  let info: { tag: string | null; text: string; id: string | null; cls: string | null } | null =
    null
  try {
    const { object } = (await target.dbg.sendCommand('DOM.resolveNode', { backendNodeId })) as {
      object: { objectId?: string }
    }
    if (object.objectId) {
      const sel = (await target.dbg.sendCommand('Runtime.callFunctionOn', {
        objectId: object.objectId,
        functionDeclaration: SELECTOR_FROM_ELEMENT_FN,
        returnByValue: true
      })) as { result: { value?: unknown } }
      if (typeof sel.result?.value === 'string') selector = sel.result.value
      const meta = (await target.dbg.sendCommand('Runtime.callFunctionOn', {
        objectId: object.objectId,
        functionDeclaration: GRAB_ELEMENT_INFO_FN,
        returnByValue: true
      })) as { result: { value?: typeof info } }
      if (meta.result?.value) info = meta.result.value
    }
  } catch {
    // Node vanished or debugger detached — fields stay null.
  }

  const ref = refForBackendNodeId(backendNodeId)

  let rect: ElementRect | null = null
  try {
    const { model } = (await target.dbg.sendCommand('DOM.getBoxModel', { backendNodeId })) as {
      model: { content: number[] }
    }
    rect = contentQuadRect(model.content)
  } catch {
    // No box model — capture the full viewport instead.
  }

  // Grab exits inspect mode before the capture so the overlay isn't in the shot.
  await stopGrab()

  let dataUrl: string | null = null
  try {
    const img =
      rect && rect.width > 0 && rect.height > 0
        ? await target.wc.capturePage(rect)
        : await target.wc.capturePage()
    dataUrl = img.toDataURL()
  } catch (e) {
    console.error('[grab] capture failed:', e)
  }

  mainWindow?.webContents.send('grab:captured', { selector, ref, rect, info, dataUrl })
}

setInspectNodeHandler((backendNodeId) =>
  grabMode ? void onGrabNodeRequested(backendNodeId) : void onPickerNodeRequested(backendNodeId)
)

// Keyboard-level shortcuts shared by the app window and every webview (see the
// before-input-event listeners). Returns true when the input was consumed —
// callers then preventDefault, which also suppresses matching menu accelerators
// so nothing fires twice.
function handleBrowserShortcut(input: Electron.Input): boolean {
  if (input.type !== 'keyDown') return false
  // Esc cancels the element picker regardless of which contents has focus —
  // the webview (the common case while hovering) or the app window.
  if (input.key === 'Escape' && pickerActive) {
    void (grabMode ? stopGrab() : stopPicker())
    return true
  }
  if (!(input.meta || input.control) || input.alt) return false
  const key = input.key.toLowerCase()
  if (key === 'r') {
    if (mainWindow && !mainWindow.webContents.isDestroyed()) {
      mainWindow.webContents.send('reload-webview', { ignoreCache: input.shift })
    }
    return true
  }
  if (input.shift) return false
  if (key === 'l') {
    // Pull focus out of the webview so the address bar can take it.
    mainWindow?.webContents.focus()
    sendBrowserCommand('focus-address')
    return true
  }
  if (key === 'f') {
    // Pull focus out of the webview so the find bar input can take it.
    mainWindow?.webContents.focus()
    sendBrowserCommand('find')
    return true
  }
  if (/^[1-9]$/.test(key)) {
    sendBrowserCommand('select-tab', { index: Number(key) })
    return true
  }
  return false
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
    {
      label: 'File',
      submenu: [
        {
          label: 'New Tab',
          accelerator: 'CmdOrCtrl+T',
          click: () => sendBrowserCommand('new-tab')
        },
        {
          label: 'Reopen Closed Tab',
          accelerator: 'CmdOrCtrl+Shift+T',
          click: () => sendBrowserCommand('reopen-tab')
        },
        { type: 'separator' },
        {
          label: 'Close Tab',
          accelerator: 'CmdOrCtrl+W',
          click: () => sendBrowserCommand('close-tab')
        },
        { label: 'Close Window', accelerator: 'CmdOrCtrl+Shift+W', role: 'close' },
        ...(isMac
          ? []
          : ([{ type: 'separator' }, { role: 'quit' }] as MenuItemConstructorOptions[]))
      ]
    },
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
    {
      label: 'History',
      submenu: [
        {
          label: 'Back',
          accelerator: isMac ? 'Cmd+[' : 'Alt+Left',
          click: () => sendBrowserCommand('back')
        },
        {
          label: 'Forward',
          accelerator: isMac ? 'Cmd+]' : 'Alt+Right',
          click: () => sendBrowserCommand('forward')
        }
      ]
    },
    {
      label: 'Window',
      submenu: [
        { role: 'minimize' },
        { role: 'zoom' },
        { type: 'separator' },
        {
          label: 'Select Next Tab',
          accelerator: 'Control+Tab',
          click: () => sendBrowserCommand('next-tab')
        },
        {
          label: 'Select Previous Tab',
          accelerator: 'Control+Shift+Tab',
          click: () => sendBrowserCommand('prev-tab')
        },
        ...(isMac
          ? ([{ type: 'separator' }, { role: 'front' }] as MenuItemConstructorOptions[])
          : [])
      ]
    }
  ]
  Menu.setApplicationMenu(Menu.buildFromTemplate(template))
}

app.whenReady().then(() => {
  installMenu()

  // Start the tool server up front rather than on the first agent spawn, and
  // publish its address. The port is OS-assigned, so without this an external
  // client — a script, a test harness, an agent running outside the app — has
  // no way to reach the tools at all, and the in-app chat is the only door.
  void startMcpServer().catch((e) => console.warn('[mcp] eager start failed:', e))

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
  // Apply the deny-all handlers to every non-default session as it's created
  // (today that's the shared persist:rever-shared partition; future isolated
  // windows get covered automatically). The app's own UI runs on the default
  // session — skip it so clipboard/etc. in the renderer keep working.
  app.on('session-created', (createdSession) => {
    if (createdSession === session.defaultSession) return
    createdSession.setPermissionRequestHandler((_wc, _permission, callback) => callback(false))
    createdSession.setPermissionCheckHandler(() => false)
  })

  // Webviews have their own webContents, so keys pressed while the page has
  // focus never reach the main window's before-input-event. Attach the same
  // shortcut interceptor to every webview.
  app.on('web-contents-created', (_event, contents) => {
    if (contents.getType() !== 'webview') return
    contents.on('before-input-event', (event, input) => {
      if (handleBrowserShortcut(input)) event.preventDefault()
    })
    // Right-click menu inside the browsed page.
    contents.on('context-menu', (_e, params) => {
      if (!mainWindow) return
      buildPageContextMenu(contents, params).popup({ window: mainWindow })
    })
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
  // Lets MCP tools request renderer-owned state (saved macros).
  initRendererBridge()

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

  // Cmd/Ctrl+W on the last remaining tab → close the window (Chromium parity).
  ipcMain.on('window:close', () => {
    mainWindow?.close()
  })

  // ── Per-tab proxy + active-partition tracking ─────────────────────────────
  // `partition` is passed once when the renderer creates an isolated proxy
  // tab; it must be registered before applyTabProxy so the proxy lands on the
  // isolated session instead of the shared one.
  ipcMain.handle(
    'proxy:set',
    async (_event, tabId: string, config: TabProxyConfig | null, partition?: string) => {
      if (partition) registerTabPartition(tabId, partition)
      await applyTabProxy(tabId, config)
      return true
    }
  )

  // The renderer reports the active tab so cookie-persistence / Chrome import
  // (which use the Electron session.cookies API, not per-webContents CDP) act
  // on the visible tab's partition.
  ipcMain.handle('tab:set-active-partition', (_event, tabId: string) => {
    setActivePartition(partitionForTab(tabId))
    return true
  })

  // Register a non-proxy tab's partition (e.g. a profile tab) so cookie import /
  // sticky-cookie snapshot / current-ip resolve to the right session.
  ipcMain.handle('tab:register-partition', (_event, tabId: string, partition: string) => {
    registerTabPartition(tabId, partition)
    return true
  })

  // ── Browser profiles ──────────────────────────────────────────────────────
  ipcMain.handle('profiles:list', () => listProfiles())
  ipcMain.handle('profiles:create', (_event, name: string, kind: ProfileKind, source?: string) =>
    createProfile(name, kind, source)
  )
  ipcMain.handle('profiles:delete', (_event, id: string) => {
    deleteProfile(id)
    return true
  })

  // ── Outbound public IP (proxy-aware) ──────────────────────────────────────
  // Fetches an IP-echo service through the given tab's session (falling back
  // to the active partition), so a tab-level proxy is reflected in the result.
  // net.request is bound to that session; its per-request 'login' event
  // answers proxy 407 challenges with the same credentials the browsing
  // session uses. Resolves to { ip } or { error } — never throws across IPC.
  ipcMain.handle('net:current-ip', async (_event, tabId?: string) => {
    const partition = tabId ? partitionForTab(tabId) : getActivePartition()
    const ses = session.fromPartition(partition)
    try {
      const ip = await new Promise<string>((resolve, reject) => {
        const req = net.request({ url: 'https://api.ipify.org?format=json', session: ses })
        const timer = setTimeout(() => {
          req.abort()
          reject(new Error('Timed out after 5s'))
        }, 5000)
        const fail = (e: Error): void => {
          clearTimeout(timer)
          reject(e)
        }
        req.on('login', (authInfo, callback) => {
          const creds = authInfo.isProxy ? proxyCredentialsForSession(ses) : null
          if (creds) callback(creds.username, creds.password)
          else callback()
        })
        req.on('error', fail)
        req.on('response', (res) => {
          let body = ''
          res.on('data', (chunk) => {
            body += chunk.toString()
          })
          res.on('error', () => fail(new Error('response stream error')))
          res.on('end', () => {
            clearTimeout(timer)
            if (res.statusCode !== 200) {
              reject(new Error(`HTTP ${res.statusCode}`))
              return
            }
            try {
              resolve(String(JSON.parse(body).ip))
            } catch {
              reject(new Error('unexpected response body'))
            }
          })
        })
        req.end()
      })
      return { ip }
    } catch (e) {
      return { error: e instanceof Error ? e.message : String(e) }
    }
  })

  // ── Workflow executor (macro replay) ──────────────────────────────────────
  ipcMain.handle('workflow:list-tools', () => listMcpTools())
  ipcMain.handle('workflow:run', async (event, steps: RunStep[], channel: string) => {
    const sender = event.sender
    return runWorkflow(steps, (progress) => {
      if (!sender.isDestroyed()) sender.send(channel, progress)
    })
  })
  ipcMain.handle(
    'workflow:run-pipeline',
    async (event, nodes: ResolvedPipeNode[], channel: string) => {
      const sender = event.sender
      await runPipeline(nodes, (progress) => {
        if (!sender.isDestroyed()) sender.send(channel, progress)
      })
      return true
    }
  )
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

  ipcMain.handle(
    'theme:set-webview-scheme',
    (_event, webContentsId: number, scheme: 'light' | 'dark' | null) =>
      setEmulatedColorScheme(webContentsId, scheme)
  )

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

  ipcMain.handle('picker:start', () => startPicker())
  ipcMain.handle('picker:stop', () => stopPicker())
  ipcMain.handle('grab:start', () => startGrab())
  ipcMain.handle('grab:stop', () => stopGrab())

  // ── Terminal (local CLI agent) ────────────────────────────────────────────
  ipcMain.handle(
    'terminal:spawn',
    (event, opts: { cols: number; rows: number; agent: 'claude' | 'shell' }) =>
      spawnTerminal(
        opts,
        (id, data) => event.sender.send(`terminal:data:${id}`, data),
        (id, code) => event.sender.send(`terminal:exit:${id}`, code)
      )
  )
  ipcMain.on('terminal:input', (_e, id: string, data: string) => writeTerminal(id, data))
  ipcMain.on('terminal:resize', (_e, id: string, cols: number, rows: number) =>
    resizeTerminal(id, cols, rows)
  )
  ipcMain.on('terminal:kill', (_e, id: string) => killTerminal(id))

  // ── /rever skill status ───────────────────────────────────────────────────
  ipcMain.handle('skill:status', () => ({ installed: skillInstalled() }))

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

  // ── Real browser cookie import (macOS) ────────────────────────────────────
  ipcMain.handle('browser-cookies:list', () => listBrowsers())
  ipcMain.handle('browser-cookies:import', (_event, opts: ImportOptions) =>
    importBrowserCookies(opts)
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
  killAllTerminals()
  if (process.platform !== 'darwin') app.quit()
})

app.on('before-quit', () => killAllTerminals())
