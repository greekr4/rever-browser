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

    // The app shell is never scrollable, even from code: an overflowing panel
    // plus scrollIntoView used to drag the whole window up.
    const shifted = await win.evaluate(() => {
      const d = document.createElement('div')
      d.style.cssText = 'position:absolute;left:0;top:1500px;width:10px;height:10px'
      document.querySelector('.chip-stack')!.parentElement!.appendChild(d)
      d.scrollIntoView({ block: 'end' })
      d.remove()
      return document.getElementById('root')!.scrollTop + document.body.scrollTop + document.documentElement.scrollTop
    })
    expect(shifted).toBe(0)

    // No layout shift: opening any panel leaves every tab where it was.
    const xs = (): Promise<number[]> => bar.locator('.chip').evaluateAll((els) => els.map((e) => Math.round(e.getBoundingClientRect().x)))
    const before = await xs()
    for (const id of ['traffic', 'websocket', 'console', 'workflows']) {
      await win.getByTestId(`chip-${id}`).click()
      // While switching, the outgoing panel is still animating out; the newest one is last.
      await expect(win.locator('.chip-panel-bottom').last()).toBeVisible()
      expect(await xs()).toEqual(before)
      if (id === 'websocket') {
        // Same shape as the other panels: toolbar on top, composer at the bottom.
        await expect(win.locator('.chip-panel-content .panel-toolbar').last()).toContainText(/Send to|보낼 곳/)
        await win.waitForTimeout(700) // let the slide-up animation settle
        const pb = (await win.locator('.chip-panel-bottom').last().boundingBox())!
        await win.screenshot({ path: path.join(SHOTS, `ws-panel-${TAG}.png`), clip: { x: pb.x, y: pb.y, width: pb.width, height: pb.height + 40 } })
      }
      // Next iteration switches straight to another panel, as a user would.
    }
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
