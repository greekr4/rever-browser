import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { test, expect, _electron as electron, type Page } from '@playwright/test'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')

async function mainWindow(app: Awaited<ReturnType<typeof electron.launch>>): Promise<Page> {
  for (let i = 0; i < 60; i++) {
    for (const w of app.windows()) {
      if ((await w.getByTestId('chat-input').count().catch(() => 0)) > 0) return w
    }
    await new Promise((r) => setTimeout(r, 500))
  }
  throw new Error('main window with the chat panel never appeared')
}

// Never clicks Import: that would raise the real Keychain prompt and copy the
// tester's own cookies.
test('browser import is offered once after onboarding and reopens from the app menu', async () => {
  test.skip(process.platform !== 'darwin', 'browser import is macOS-only')
  const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'rever-import-'))
  const app = await electron.launch({
    args: ['.'],
    cwd: ROOT,
    env: { ...process.env, REVER_USER_DATA_DIR: userData }
  })
  try {
    let win = await mainWindow(app)
    await win.evaluate(() => localStorage.setItem('rev:lang', 'en'))
    await win.reload()
    win = await mainWindow(app)
    const importDialog = win.getByRole('dialog', { name: 'Import from your browser' })

    // First run: provider onboarding comes first, the import offer waits for it.
    await expect(win.getByRole('dialog', { name: 'Connect your AI provider' })).toBeVisible()
    await expect(importDialog).toHaveCount(0)
    await win.getByRole('button', { name: 'Skip for now' }).click()
    await expect(importDialog).toBeVisible()

    // Dismissed offers stay dismissed across launches.
    await importDialog.getByRole('button', { name: 'Not now' }).click()
    await expect(importDialog).toHaveCount(0)
    await win.reload()
    win = await mainWindow(app)
    await expect(win.getByRole('dialog', { name: 'Import from your browser' })).toHaveCount(0)

    // App menu → Import from Browser… reopens it.
    await app.evaluate(({ Menu }) => {
      const appMenu = Menu.getApplicationMenu()?.items[0]
      appMenu?.submenu?.items.find((i) => i.label === 'Import from Browser…')?.click()
    })
    await expect(win.getByRole('dialog', { name: 'Import from your browser' })).toBeVisible()
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
