import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { test, expect, _electron as electron, type Page } from '@playwright/test'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
// Captures for the A/B report; test-image/ is gitignored.
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

test('크롬형 툴바: 설정 버튼은 ⋮ 메뉴 하나로 모이고 메뉴 항목이 동작한다', async () => {
  const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'rever-toolbar-'))
  const app = await electron.launch({ args: ['.'], cwd: ROOT, env: { ...process.env, REVER_USER_DATA_DIR: userData } })
  try {
    let win = await mainWindow(app)
    await win.evaluate(() => localStorage.setItem('rev:lang', 'en'))
    await win.reload()
    win = await mainWindow(app)
    await win.getByRole('button', { name: 'Skip for now' }).click()
    const importDialog = win.getByRole('dialog', { name: 'Import from your browser' })
    if (await importDialog.isVisible().catch(() => false)) await importDialog.getByRole('button', { name: 'Not now' }).click()

    // The old scattered buttons are gone from the frame.
    for (const gone of ['Embedded', 'External', 'Desktop', 'Profile']) {
      await expect(win.getByRole('button', { name: gone, exact: true })).toHaveCount(0)
    }
    await expect(win.getByRole('button', { name: 'New tab', exact: true })).toBeVisible()
    await expect(win.getByRole('button', { name: 'New tab in profile' })).toBeVisible()

    const menuBtn = win.getByRole('button', { name: 'Customize and control Rever Browser' })
    await menuBtn.click()
    const menu = win.getByRole('menu')
    await expect(menu).toBeVisible()
    for (const item of ['Theme', 'Page theme', 'Language', 'Notifications', 'Viewport', 'View page source']) {
      await expect(menu.getByRole('menuitem', { name: new RegExp(`^${item}`) })).toBeVisible()
    }
    await expect(menu.getByRole('menuitem', { name: /^Browser mode/ })).toHaveCount(0)
    await expect(menu).toContainText(/Rever Browser v\d+\.\d+\.\d+/)
    await win.screenshot({ path: path.join(SHOTS, 'chrome-menu-open.png') })

    // Cycling items keep the menu open and update their value.
    const lang = menu.getByRole('menuitem', { name: /^Language/ })
    await expect(lang).toContainText('English')
    await lang.click()
    await expect(menu.getByRole('menuitem', { name: /^언어|^Language/ })).toContainText('한국어')
    await win.keyboard.press('Escape')

    // Language is now Korean.
    await win.getByRole('button', { name: '프로필로 새 탭 열기' }).click()
    await win.screenshot({ path: path.join(SHOTS, 'chrome-profile-open.png') })
    await win.keyboard.press('Escape').catch(() => {})
    await win.mouse.click(5, 300)

    // Dark theme gets its own frame colours.
    await win.getByRole('button', { name: 'Rever Browser 설정 및 제어' }).click()
    const theme = win.getByRole('menu').getByRole('menuitem', { name: /^테마/ })
    for (let i = 0; i < 3 && !(await theme.textContent())?.includes('다크'); i++) await theme.click()
    await expect(theme).toContainText('다크')
    await win.screenshot({ path: path.join(SHOTS, 'chrome-dark.png') })
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
