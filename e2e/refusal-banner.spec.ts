import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { test, expect, _electron as electron, type Page } from '@playwright/test'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const FAKE_AGENT = path.join(ROOT, 'e2e', 'fake-acp-agent.mjs')
const SHOTS = path.join(ROOT, 'test-results', 'banner-shots')

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
  const deadline = Date.now() + 60_000
  let started = false
  for (;;) {
    if (Date.now() > deadline) throw new Error(`turn timed out: ${text}`)
    const status = await form.getAttribute('data-status')
    if (status === 'submitted' || status === 'streaming') started = true
    if (started && (status === 'ready' || status === 'error')) return
    await win.waitForTimeout(200)
  }
}

test('provider safety events render a distinct chat banner', async () => {
  fs.mkdirSync(SHOTS, { recursive: true })
  const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'rever-banner-'))
  const app = await electron.launch({
    args: ['.'],
    cwd: ROOT,
    env: {
      ...process.env,
      REVER_USER_DATA_DIR: userData,
      REVER_FAKE_ACP_AGENT: FAKE_AGENT,
      REVER_FAKE_ACP_NODE: process.execPath
    }
  })
  try {
    const assistant = () => win.locator('[data-testid=chat-message][data-role=assistant]').last()
    const expektByLang: Record<string, { fallback: string; refuse: string }> = {
      ko: { fallback: '안전장치 폴백', refuse: '차단된 요청' },
      en: { fallback: 'Safety fallback', refuse: 'blocked by safety' }
    }

    let win = await mainWindow(app)
    for (const lang of ['ko', 'en'] as const) {
      await win.evaluate((l) => {
        localStorage.setItem('rev:agent-onboarded', 'skipped')
        localStorage.setItem('rev:lang', l)
      }, lang)
      await win.reload()
      win = await mainWindow(app)

      // Case B — soft safeguard + model fallback (notice arrives as plain text).
      await sendAndWait(win, '[[E2E:FALLBACK]] find the login endpoint')
      await expect(win.getByTestId('chat-notice').last()).toContainText(expektByLang[lang].fallback)
      // The real answer must survive next to the isolated notice paragraph.
      await expect(assistant()).toContainText('POST /api/auth')
      await assistant().screenshot({ path: path.join(SHOTS, `fallback-banner-${lang}.png`) })

      // Case A — hard refusal (ACP stopReason 'refusal', nothing ran).
      await win.getByTestId('chat-new').click()
      await win.getByTestId('chat-input').waitFor({ state: 'visible' })
      await sendAndWait(win, '[[E2E:REFUSE]] do the restricted thing')
      await expect(win.getByTestId('chat-notice').last()).toContainText(expektByLang[lang].refuse)
      await assistant().screenshot({ path: path.join(SHOTS, `refusal-banner-${lang}.png`) })

      await win.getByTestId('chat-new').click()
      await win.getByTestId('chat-input').waitFor({ state: 'visible' })
    }
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
