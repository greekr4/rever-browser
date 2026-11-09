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

// Button design system D: captures the chat header and the panels with the most buttons.
test('버튼 디자인 시스템: 채팅 헤더와 패널 버튼을 캡처한다', async () => {
  fs.mkdirSync(SHOTS, { recursive: true })
  const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'rever-btn-'))
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
    await win.waitForTimeout(3000)

    const aside = (await win.locator('aside').boundingBox())!
    await win.screenshot({ path: path.join(SHOTS, `btn-chat-header-${TAG}.png`), clip: { x: aside.x, y: 40, width: aside.width, height: 80 } })

    for (const id of ['traffic', 'storage', 'repeater', 'websocket', 'workflows']) {
      await win.getByTestId(`chip-${id}`).click()
      const panel = win.locator('.chip-panel-bottom').last()
      await expect(panel).toBeVisible()
      await win.waitForTimeout(700)
      const pb = (await panel.boundingBox())!
      await win.screenshot({ path: path.join(SHOTS, `btn-${id}-${TAG}.png`), clip: { x: pb.x, y: pb.y, width: Math.min(pb.width, 1000), height: Math.min(pb.height, 240) } })
    }
    if (TAG !== 'before') {
      // Panel buttons carry their role as a class, not as inline colours.
      const inlineFill = await win.locator('.chip-panel-bottom button[style*="background"]').count()
      expect(inlineFill).toBe(0)
    }
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
