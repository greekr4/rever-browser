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
      localStorage.setItem('rev:lang', 'en')
    })
    await win.reload()
    win = await mainWindow(app)

    await sendAndWait(win, 'Call the list_requests tool with {"limit": 1} exactly once, then reply with only the word DONE.')
    const assistant = win.locator('[data-testid=chat-message][data-role=assistant]').last()
    await expect(assistant).toContainText('DONE')
    await expect(win.getByTestId('chat-form')).toHaveAttribute('data-status', 'ready')
    await assistant.screenshot({ path: path.join(SHOTS, 'claude-cli-chat.png') })
    await expect(assistant).toContainText(/tools?\s*1|list_requests/i)
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
