import fs from 'node:fs'
import http from 'node:http'
import type { AddressInfo } from 'node:net'
import os from 'node:os'
import path from 'node:path'

import { test, expect, _electron as electron, type Page } from '@playwright/test'

import { ROOT } from './fixtures'

// External-browser mode with the Servo shell in place of Chrome. Build it first:
//   cd servo-shell && ./apply-patches.sh && cargo build
const SHELL = path.join(ROOT, 'servo-shell/target/debug/rever-servo-shell')
const SHOTS = path.join(ROOT, 'test-image')

const PAGE = `<!doctype html><title>servo external fixture</title><h1>servo external fixture</h1>
<script>
fetch('/api/items').then(function (r) { return r.json() })
var x = new XMLHttpRequest(); x.open('POST', '/api/login'); x.send('user=rever&pass=servo')
</script>`

/** A page that makes one GET and one POST; the POST's body is echoed back. */
function startFixture(): Promise<http.Server> {
  const server = http.createServer((req, res) => {
    let body = ''
    req.on('data', (chunk) => (body += chunk))
    req.on('end', () => {
      if (req.url === '/api/items') return res.setHeader('content-type', 'application/json').end('{"items":[1,2,3]}')
      if (req.url === '/api/login') return res.setHeader('content-type', 'application/json').end(JSON.stringify({ echoed: body }))
      res.setHeader('content-type', 'text/html').end(PAGE)
    })
  })
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)))
}

async function mainWindow(app: Awaited<ReturnType<typeof electron.launch>>): Promise<Page> {
  for (let i = 0; i < 60; i++) {
    for (const w of app.windows()) {
      if ((await w.getByTestId('chat-input').count().catch(() => 0)) > 0) return w
    }
    await new Promise((r) => setTimeout(r, 500))
  }
  throw new Error('main window never appeared')
}

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

test('Servo 외부 모드: Servo 셸에 붙어 요청·응답 본문을 트래픽 저장소에 기록한다', async () => {
  test.skip(!fs.existsSync(SHELL), 'build servo-shell first (see the top of this file)')
  const fixture = await startFixture()
  const origin = `http://127.0.0.1:${(fixture.address() as AddressInfo).port}`
  const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'rever-servo-'))
  const app = await electron.launch({ args: ['.'], cwd: ROOT, env: { ...process.env, REVER_USER_DATA_DIR: userData, REVER_SERVO_SHELL: SHELL } })
  try {
    let win = await mainWindow(app)
    await win.evaluate(() => {
      localStorage.setItem('rev:agent-onboarded', 'skipped')
      localStorage.setItem('rev:browser-import-offered', '1')
      localStorage.setItem('rev:browser-mode', JSON.stringify({ state: { mode: 'external' }, version: 0 }))
    })
    await win.reload()
    win = await mainWindow(app)

    // The renderer starts the external browser on mount; navigate once it is attached.
    await expect(async () => {
      // `window.rev` is the preload API; its type lives in the renderer's tsconfig, not this one.
      await win.evaluate(
        (url) => (window as unknown as { rev: { external: { navigate(u: string): Promise<void> } } }).rev.external.navigate(url),
        `${origin}/`
      )
    }).toPass({ timeout: 30_000 })

    await expect(async () => {
      const list = await mcpCall(userData, 'list_requests', {})
      expect(list).toContain('/api/items')
      expect(list).toContain('/api/login')
    }).toPass({ timeout: 20_000 })

    // The bodies are what the Servo embedding API does not expose; they come from the servo-net tap patch.
    const list = await mcpCall(userData, 'list_requests', {})
    const ids = [...new Set(list.match(/[0-9a-f]{8}-[0-9a-f-]{27}/g) ?? [])]
    await expect(async () => {
      const details = await Promise.all(ids.map((requestId) => mcpCall(userData, 'get_request', { requestId, full: true })))
      const login = details.find((d) => d.includes('/api/login')) ?? ''
      expect(login).toContain('user=rever&pass=servo') // request body
      expect(login).toContain('echoed') // response body
    }).toPass({ timeout: 10_000 })

    fs.mkdirSync(SHOTS, { recursive: true })
    await win.waitForTimeout(1500) // let a screencast frame land before the screenshot
    await win.screenshot({ path: path.join(SHOTS, 'servo-external.png') })
  } finally {
    await app.close()
    fixture.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
