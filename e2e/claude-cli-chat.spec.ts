import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { test, expect, _electron as electron, type Page } from '@playwright/test'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const SHOTS = path.join(ROOT, 'test-image')

async function mainWindow(app: Awaited<ReturnType<typeof electron.launch>>): Promise<Page> {
  for (let i = 0; i < 60; i++) {
    for (const w of app.windows()) {
      if ((await w.getByTestId('chat-input').count().catch(() => 0)) > 0) return w
    }
    await new Promise((r) => setTimeout(r, 500))
  }
  throw new Error('main window with the chat panel never appeared')
}

async function sendAndWait(win: Page, text: string): Promise<void> {
  const form = win.getByTestId('chat-form')
  await win.getByTestId('chat-input').fill(text)
  await win.getByTestId('chat-input').press('Enter')
  const deadline = Date.now() + 180_000
  let started = false
  for (;;) {
    if (Date.now() > deadline) throw new Error(`turn timed out: ${text}`)
    const status = await form.getAttribute('data-status')
    if (status === 'submitted' || status === 'streaming') started = true
    if (started && (status === 'ready' || status === 'error')) return
    await win.waitForTimeout(300)
  }
}

// Spends one real turn on whatever the local `claude` CLI is signed in with.
test('Claude Code가 ACP 없이 CLI로 직접 돌아 MCP 도구를 쓰고 답한다', async () => {
  test.skip(!process.env.REVER_E2E_REAL_CLAUDE, 'set REVER_E2E_REAL_CLAUDE=1 to spend a real Claude Code turn')
  fs.mkdirSync(SHOTS, { recursive: true })
  const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'rever-claude-cli-'))
  const app = await electron.launch({ args: ['.'], cwd: ROOT, env: { ...process.env, REVER_USER_DATA_DIR: userData } })
  try {
    let win = await mainWindow(app)
    await win.evaluate(() => {
      localStorage.setItem('rev:agent-onboarded', 'skipped')
      localStorage.setItem('rev:browser-import-offered', '1')
      localStorage.setItem('rev:lang', 'en')
    })
    await win.reload()
    win = await mainWindow(app)

    // Launch probes list models from the CLIs themselves (no prompt) and cache them on disk.
    const cacheFile = path.join(userData, 'model-catalog.json')
    await expect.poll(() => fs.existsSync(cacheFile), { timeout: 40_000 }).toBe(true)
    // Codex is listed too when installed; give its probe a moment (Claude usually lands first).
    await expect
      .poll(() => Object.keys(JSON.parse(fs.readFileSync(cacheFile, 'utf8')).agents).length, { timeout: 20_000 })
      .toBeGreaterThan(1)
      .catch(() => {})
    const cache = JSON.parse(fs.readFileSync(cacheFile, 'utf8'))
    const claudeModels = cache.agents['claude-code']?.models ?? []
    expect(claudeModels.length).toBeGreaterThan(0)
    // Before the first message, the model chip already offers the live list.
    await win.getByTestId('model-chip').click()
    await expect(win.getByTestId('model-menu').getByRole('menuitemradio', { name: claudeModels[1]?.name ?? claudeModels[0].name }).first()).toBeVisible()
    await win.getByTestId('model-menu').screenshot({ path: path.join(SHOTS, 'live-catalog-menu.png') })
    await win.keyboard.press('Escape')
    console.log('catalog:', Object.fromEntries(Object.entries(cache.agents).map(([k, v]) => [k, (v as { models: { name: string }[] }).models.map((m) => m.name).slice(0, 5)])))

    await sendAndWait(win, 'Call the list_requests tool with {"limit": 1} exactly once, then reply with only the word DONE.')
    const assistant = win.locator('[data-testid=chat-message][data-role=assistant]').last()
    await expect(assistant).toContainText('DONE')
    await expect(win.getByTestId('chat-form')).toHaveAttribute('data-status', 'ready')
    await assistant.screenshot({ path: path.join(SHOTS, 'claude-cli-chat.png') })
    await expect(assistant).toContainText(/tools?\s*1|list_requests/i)

    // Live session: the model chip lists the CLI's own models and changes effort / fast mode in place.
    await win.getByTestId('model-chip').click()
    const menu = win.getByTestId('model-menu')
    await expect(menu.getByRole('menuitemradio').first()).toBeVisible()
    await menu.getByRole('radio', { name: 'Low', exact: true }).click()
    await expect(menu.getByRole('radio', { name: 'Low', exact: true })).toHaveAttribute('aria-checked', 'true')
    await expect(win.getByTestId('model-chip')).toContainText('Low')
    const fast = menu.getByRole('switch')
    if (await fast.count()) {
      await fast.check()
      await expect(fast).toBeChecked()
      await expect(win.getByTestId('model-chip')).toContainText('⚡')
    }
    await menu.screenshot({ path: path.join(SHOTS, 'claude-cli-model-menu.png') })
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
