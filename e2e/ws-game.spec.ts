import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { test, expect, _electron as electron, type Page } from '@playwright/test'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const SHOTS = path.join(ROOT, 'test-image')
const GAME = 'http://127.0.0.1:8777/ws-game.html'

async function mainWindow(app: Awaited<ReturnType<typeof electron.launch>>): Promise<Page> {
  for (let i = 0; i < 60; i++) {
    for (const w of app.windows()) {
      if ((await w.getByTestId('chat-input').count().catch(() => 0)) > 0) return w
    }
    await new Promise((r) => setTimeout(r, 500))
  }
  throw new Error('main window never appeared')
}

// Reads the page through the app's own MCP endpoint (same text an agent sees).
async function snapshotText(userData: string): Promise<string> {
  const ep = JSON.parse(fs.readFileSync(path.join(userData, 'mcp-endpoint.json'), 'utf8'))
  const h: Record<string, string> = { 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream', Authorization: `Bearer ${ep.token}`, 'X-Rever-Catalog': 'full' }
  const init = await fetch(ep.url, { method: 'POST', headers: h, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-03-26', capabilities: {}, clientInfo: { name: 'e2e', version: '1' } } }) })
  h['mcp-session-id'] = init.headers.get('mcp-session-id') ?? ''
  await init.text()
  await fetch(ep.url, { method: 'POST', headers: h, body: JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) })
  const r = await fetch(ep.url, { method: 'POST', headers: h, body: JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'browser_snapshot', arguments: { full: true } } }) })
  return r.text()
}

test('게임 월드: 조작한 프레임에 서버가 실제로 반응한다 (채팅 전파 · 범위 거부 · 숨은 텔레포트)', async () => {
  // Needs the static fixture server on 8777 (docs/agent-testing.md); 8779 comes from global setup.
  const up = await fetch(GAME).then((r) => r.ok).catch(() => false)
  test.skip(!up, 'serve test-fixtures on 8777 first')
  fs.mkdirSync(SHOTS, { recursive: true })
  const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'rever-wsgame-'))
  const app = await electron.launch({ args: ['.'], cwd: ROOT, env: { ...process.env, REVER_USER_DATA_DIR: userData } })
  try {
    let win = await mainWindow(app)
    await win.evaluate(() => {
      localStorage.setItem('rev:agent-onboarded', 'skipped')
      localStorage.setItem('rev:browser-import-offered', '1')
      localStorage.setItem('rev:lang', 'ko')
    })
    await win.reload()
    win = await mainWindow(app)
    await win.locator('input.omnibox').fill(GAME)
    await win.locator('input.omnibox').press('Enter')
    await win.waitForTimeout(3500)

    await win.getByTestId('chip-websocket').click()
    const panel = win.locator('.chip-panel-bottom').last()
    await panel.getByText('/world?room=lobby', { exact: false }).first().click()
    await expect(panel.getByTestId('ws-live')).toContainText('●', { timeout: 6000 })
    const composer = panel.getByRole('textbox', { name: /페이로드|Payload/ })
    for (const msg of ['{"type":"chat","text":"rever"}', '{"type":"move","x":30,"y":11111}', '{"type":"teleport","x":95,"y":5}']) {
      await composer.fill(msg)
      await composer.press('Enter')
      await win.waitForTimeout(700)
    }
    await win.screenshot({ path: path.join(SHOTS, 'ws-game-after.png') })

    const snap = await snapshotText(userData)
    expect(snap).toContain('player-1: rever') // A1: the server broadcast the chat
    expect(snap).toContain('REJECTED — server validates bounds') // A2
    expect(snap).toContain('FOUND — teleport accepted with no checks') // A3
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
