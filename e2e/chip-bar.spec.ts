import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { test, expect, _electron as electron, type Page } from '@playwright/test'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const SHOTS = path.join(ROOT, 'test-image')
const TAG = process.env.CHAT_SHOT_TAG ?? 'after'

async function mainWindow(app: Awaited<ReturnType<typeof electron.launch>>): Promise<Page> {
  for (let i = 0; i < 60; i++) {
    for (const w of app.windows()) {
      if ((await w.getByTestId('chat-input').count().catch(() => 0)) > 0) return w
    }
    await new Promise((r) => setTimeout(r, 500))
  }
  throw new Error('main window never appeared')
}

test('하단 패널 탭 줄을 캡처하고 열고 닫기를 확인한다', async () => {
  fs.mkdirSync(SHOTS, { recursive: true })
  const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'rever-chips-'))
  const app = await electron.launch({ args: ['.'], cwd: ROOT, env: { ...process.env, REVER_USER_DATA_DIR: userData } })
  try {
    let win = await mainWindow(app)
    await win.evaluate((lang) => {
      localStorage.setItem('rev:agent-onboarded', 'skipped')
      localStorage.setItem('rev:browser-import-offered', '1')
      localStorage.setItem('rev:lang', lang)
    }, process.env.CHIP_LANG === 'ko' ? 'ko' : 'en')
    await win.reload()
    win = await mainWindow(app)
    // Some traffic so the count badges show.
    await win.waitForTimeout(4000)
    const bar = win.locator('.chip-stack')
    await expect(bar).toBeVisible()
    await bar.screenshot({ path: path.join(SHOTS, `chip-bar-${TAG}.png`) })

    // Open a panel: its tab shows as active, and clicking it again closes it.
    const traffic = bar.getByRole('button', { name: /Traffic|트래픽/ })
    await traffic.click()
    await expect(win.locator('.chip-panel-bottom')).toBeVisible()
    const box = (await bar.boundingBox())!
    await win.screenshot({ path: path.join(SHOTS, `chip-bar-open-${TAG}.png`), clip: { x: box.x, y: box.y - 220, width: Math.min(box.width, 1100), height: 220 + box.height } })
    await traffic.click()
    await expect(win.locator('.chip-panel-bottom')).toHaveCount(0)
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
