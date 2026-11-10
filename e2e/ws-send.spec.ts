import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { test, expect, _electron as electron, type Page } from '@playwright/test'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const SHOTS = path.join(ROOT, 'test-image')
const WORLD = 'http://127.0.0.1:8777/ws-world.html'

async function mainWindow(app: Awaited<ReturnType<typeof electron.launch>>): Promise<Page> {
  for (let i = 0; i < 60; i++) {
    for (const w of app.windows()) {
      if ((await w.getByTestId('chat-input').count().catch(() => 0)) > 0) return w
    }
    await new Promise((r) => setTimeout(r, 500))
  }
  throw new Error('main window never appeared')
}

// Reads and drives the page (and its cross-site frame) through the app's own MCP endpoint.
async function mcpCall(userData: string, name: string, args: object): Promise<string> {
  const ep = JSON.parse(fs.readFileSync(path.join(userData, 'mcp-endpoint.json'), 'utf8'))
  const h: Record<string, string> = { 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream', Authorization: `Bearer ${ep.token}`, 'X-Rever-Catalog': 'full' }
  const init = await fetch(ep.url, { method: 'POST', headers: h, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-03-26', capabilities: {}, clientInfo: { name: 'e2e', version: '1' } } }) })
  h['mcp-session-id'] = init.headers.get('mcp-session-id') ?? ''
  await init.text()
  await fetch(ep.url, { method: 'POST', headers: h, body: JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) })
  const r = await fetch(ep.url, { method: 'POST', headers: h, body: JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name, arguments: args } }) })
  return r.text()
}

// Launches the app on the WS fixture page. Skips the test when the static fixture servers are down.
async function openWorld(): Promise<{ app: Awaited<ReturnType<typeof electron.launch>>; win: Page; userData: string }> {
  // Needs the static fixture servers on 8777/8778 (docs/agent-testing.md); 8779 comes from global setup.
  const up = await fetch(WORLD).then((r) => r.ok).catch(() => false)
  test.skip(!up, 'serve test-fixtures on 8777 and 8778 first')
  fs.mkdirSync(SHOTS, { recursive: true })
  const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'rever-wssend-'))
  const app = await electron.launch({ args: ['.'], cwd: ROOT, env: { ...process.env, REVER_USER_DATA_DIR: userData } })
  let win = await mainWindow(app)
  await win.evaluate(() => {
    localStorage.setItem('rev:agent-onboarded', 'skipped')
    localStorage.setItem('rev:browser-import-offered', '1')
    localStorage.setItem('rev:lang', 'ko')
  })
  await win.reload()
  win = await mainWindow(app)
  await win.locator('input.omnibox').fill(WORLD)
  await win.locator('input.omnibox').press('Enter')
  await win.waitForTimeout(4000) // both sockets open at load, before anything can hook them
  return { app, win, userData }
}

test('웹소켓: 페이지가 이미 연 소켓(페이지·다른 사이트 iframe)으로 새로고침 없이 보내고, 닫힌 소켓은 재연결해 보낸다', async () => {
  const { app, win, userData } = await openWorld()
  try {
    await win.getByTestId('chip-websocket').click()
    const panel = win.locator('.chip-panel-bottom').last()
    const composer = panel.getByRole('textbox', { name: /페이로드|Payload/ })

    for (const [conn, msg] of [['/ws?room=lobby', '{"type":"chat","text":"rever"}'], ['/ws?widget=chat', 'hello-widget']] as const) {
      await panel.getByText(conn, { exact: false }).first().click()
      await expect(panel.getByTestId('ws-live')).toContainText('●', { timeout: 6000 })
      await composer.fill(msg)
      await composer.press('Enter')
      await expect(panel.getByText(/^(전송됨|Sent)$/)).toBeVisible()
      await win.waitForTimeout(800)
    }
    await win.screenshot({ path: path.join(SHOTS, 'ws-send-after.png') })

    // The page and the iframe each print PASS once they get an echo of a message they did not send.
    const snap = await mcpCall(userData, 'browser_snapshot', { full: true })
    expect(snap).toContain('rever')
    expect(snap.match(/PASS/g)?.length ?? 0).toBeGreaterThanOrEqual(2)

    // Reconnect: once the page's socket is closed, Send reopens it from the page and
    // the panel follows the new connection, so the server's echo is on screen.
    await panel.getByText('/ws?room=lobby', { exact: false }).first().click()
    await mcpCall(userData, 'browser_evaluate', { expression: 'ws.close()' })
    const sendBtn = panel.getByTestId('ws-send')
    await expect(sendBtn).toHaveText(/재연결 후 전송|Reconnect & send/, { timeout: 6000 })
    await composer.fill('back-after-drop')
    await composer.press('Enter')
    await expect(panel.getByText(/^(재연결 후 전송됨|Reconnected and sent)$/)).toBeVisible()
    await expect(panel.getByText('"received":"back-after-drop"', { exact: false })).toBeVisible()
    await expect(sendBtn).toHaveText(/^(전송|Send)$/, { timeout: 6000 })
    await win.screenshot({ path: path.join(SHOTS, 'ws-reconnect-after.png') })
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})

test('WS 리피터: 보낸 프레임을 고쳐 다시 보내고, 끊긴 소켓에서 시퀀스를 재생한다', async () => {
  const { app, win, userData } = await openWorld()
  try {
    await win.getByTestId('chip-wsrepeater').click()
    const panel = win.locator('.chip-panel-bottom').last()
    const editor = panel.getByRole('textbox', { name: /페이로드|Payload/ })
    const replies = panel.getByTestId('wsr-replies')
    const status = panel.getByTestId('wsr-status')
    await panel.getByTestId('wsr-socket').selectOption('ws://127.0.0.1:8779/ws?room=lobby')

    // A frame the page sent loads as the read-only original and as the draft to edit.
    await panel.getByTestId('wsr-sent').getByText('"type":"join"', { exact: false }).first().click()
    await expect(editor).toHaveValue('{"type":"join","name":"player-1"}')
    await editor.fill('{"type":"join","name":"rever-edit"}')
    await panel.getByTestId('wsr-send').click()
    await expect(status).toHaveText(/^(전송됨|Sent)$/)
    // Only what arrived after the send, each with the time since it.
    await expect(replies.getByText('rever-edit', { exact: false })).toBeVisible()
    await expect(replies.getByText(/^\+\d+ms$/).first()).toBeVisible()

    // Sequence on a dropped socket: the first frame reconnects, the rest follow on the new connection.
    for (const p of ['seq-1', 'seq-2']) {
      await editor.fill(p)
      await panel.getByTestId('wsr-add-step').click()
    }
    await mcpCall(userData, 'browser_evaluate', { expression: 'ws.close()' })
    await expect(panel.getByTestId('wsr-send')).toHaveText(/재연결 후 전송|Reconnect & send/, { timeout: 6000 })
    await panel.getByTestId('wsr-sequence').getByRole('button', { name: /재생|Play/ }).click()
    await expect(status).toHaveText(/시퀀스 전송 완료 \(2개 프레임\)|Sequence sent \(2 frames\)/)
    await expect(replies.getByText('"type":"hello"', { exact: false })).toBeVisible()
    await expect(replies.getByText('"received":"seq-1"', { exact: false })).toBeVisible()
    await expect(replies.getByText('"received":"seq-2"', { exact: false })).toBeVisible()
    await win.screenshot({ path: path.join(SHOTS, 'ws-repeater-after.png') })
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
