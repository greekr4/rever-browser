import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { test, expect, _electron as electron, type Page } from '@playwright/test'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const FAKE_AGENT = path.join(ROOT, 'e2e', 'fake-acp-agent.mjs')
const SHOTS = path.join(ROOT, 'test-image')
const TAG = process.env.CHAT_SHOT_TAG ?? 'after'

async function mainWindow(app: Awaited<ReturnType<typeof electron.launch>>): Promise<Page> {
  for (let i = 0; i < 60; i++) {
    for (const w of app.windows()) {
      if ((await w.getByTestId('chat-input').count().catch(() => 0)) > 0) return w
    }
    await new Promise((r) => setTimeout(r, 500))
  }
  throw new Error('main window with the chat panel never appeared')
}

// Captures one long mixed-markdown answer for typography A/B; light and dark.
test('채팅 본문 타이포그래피를 라이트·다크로 캡처한다', async () => {
  fs.mkdirSync(SHOTS, { recursive: true })
  const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'rever-typo-'))
  const app = await electron.launch({
    args: ['.'],
    cwd: ROOT,
    env: { ...process.env, REVER_USER_DATA_DIR: userData, REVER_FAKE_ACP_AGENT: FAKE_AGENT, REVER_FAKE_ACP_NODE: process.execPath }
  })
  try {
    let win = await mainWindow(app)
    await win.evaluate(() => {
      localStorage.setItem('rev:agent-onboarded', 'skipped')
      localStorage.setItem('rev:browser-import-offered', '1')
      localStorage.setItem('rev:lang', 'ko')
      localStorage.setItem('rev:chat-w', '440')
    })
    await win.reload()
    win = await mainWindow(app)
    const input = win.getByTestId('chat-input')
    await input.fill('[[E2E:RICH]] 로그인 흐름이랑 서명 방식 정리해줘')
    await input.press('Enter')
    const answer = win.locator('[data-testid=chat-message][data-role=assistant]').last()
    await expect(answer).toContainText('권한 검사가 있는지')
    await input.fill('다음 질문 초안')
    const aside = (await win.locator('aside').boundingBox())!
    const clip = { x: aside.x, y: 0, width: aside.width, height: aside.height + aside.y }
    await win.screenshot({ path: path.join(SHOTS, `chat-typo-light-${TAG}.png`), clip })
    await win.evaluate(() => document.documentElement.setAttribute('data-theme', 'dark'))
    await win.screenshot({ path: path.join(SHOTS, `chat-typo-dark-${TAG}.png`), clip })
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
