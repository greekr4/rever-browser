import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { test, expect, _electron as electron, type Page } from '@playwright/test'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const FAKE_AGENT = path.join(ROOT, 'e2e', 'fake-acp-agent.mjs')
const SHOTS = path.join(ROOT, 'test-image')
// before/after captures for the A/B report: CHAT_SHOT_TAG=before|after
const TAG = process.env.CHAT_SHOT_TAG ?? 'after'
// Chat panel width; 300 is the resizer's minimum, the hardest case for the composer row.
const CHAT_W = process.env.CHAT_W ?? '300'

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
  // Several session tabs can be mounted; only the visible one is in play.
  const panel = win.locator('[role=tabpanel]:visible')
  const form = panel.getByTestId('chat-form')
  await panel.getByTestId('chat-input').fill(text)
  await panel.getByTestId('chat-input').press('Enter')
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

async function shotPanel(win: Page, name: string): Promise<void> {
  const form = await win.locator('[role=tabpanel]:visible').getByTestId('chat-form').boundingBox()
  const vp = await win.evaluate(() => ({ h: window.innerHeight }))
  if (!form) throw new Error('chat form not visible')
  await win.screenshot({ path: path.join(SHOTS, name), clip: { x: form.x - 13, y: 0, width: form.width + 26, height: vp.h } })
}

test('채팅 패널: 대화와 입력창을 캡처하고 기본 동작을 확인한다', async () => {
  fs.mkdirSync(SHOTS, { recursive: true })
  const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'rever-composer-'))
  const app = await electron.launch({
    args: ['.'],
    cwd: ROOT,
    env: { ...process.env, REVER_USER_DATA_DIR: userData, REVER_FAKE_ACP_AGENT: FAKE_AGENT, REVER_FAKE_ACP_NODE: process.execPath }
  })
  try {
    let win = await mainWindow(app)
    await win.evaluate((w) => {
      localStorage.setItem('rev:agent-onboarded', 'skipped')
      localStorage.setItem('rev:browser-import-offered', '1')
      localStorage.setItem('rev:lang', 'ko')
      // Narrowest chat panel: the composer row must still fit its send button.
      localStorage.setItem('rev:chat-w', w)
    }, CHAT_W)
    await win.reload()
    win = await mainWindow(app)
    await shotPanel(win, `chat-empty-${TAG}.png`)
    if (TAG !== 'before') {
      const card = (await win.getByTestId('chat-form').boundingBox())!
      const send = (await win.getByRole('button', { name: '보내기' }).boundingBox())!
      expect(send.x + send.width).toBeLessThanOrEqual(card.x + card.width)
    }

    await sendAndWait(win, '[[E2E:TOOLS]] 로그인 API 찾아줘')
    await expect(win.locator('[data-testid=chat-message][data-role=assistant]').last()).toContainText('POST /api/v3/login')
    await win.getByTestId('chat-input').fill('여러 줄 입력\n두 번째 줄')
    await shotPanel(win, `chat-conversation-${TAG}.png`)
    if (TAG === 'before') return

    // User messages are right-aligned bubbles; the assistant reply is full-width.
    const user = await win.locator('[data-testid=chat-message][data-role=user]').last().boundingBox()
    const form = await win.getByTestId('chat-form').boundingBox()
    // A short message's bubble hugs the right edge instead of starting at the left.
    expect(user!.x).toBeGreaterThan(form!.x + 30)

    // Permission chip: opens upward, switches mode.
    const perm = win.getByTestId('chat-permission')
    await expect(perm).toHaveAttribute('aria-label', /자동 승인/)
    await perm.click()
    await expect(win.getByRole('menuitemradio', { name: /매번 확인/ })).toBeVisible()
    await shotPanel(win, `chat-permission-menu-${TAG}.png`)
    await win.getByRole('menuitemradio', { name: /매번 확인/ }).click()
    await expect(perm).toHaveAttribute('aria-label', /매번 확인/)

    // Tool profile chip.
    await win.getByTestId('tool-profile').click()
    await win.getByTestId('tool-profile-probe').click()
    await expect(win.getByTestId('tool-profile')).toHaveAttribute('aria-label', /Access/)

    // Send is disabled for an empty input.
    await win.getByTestId('chat-input').fill('')
    await expect(win.getByRole('button', { name: '보내기' })).toBeDisabled()

    // Model chip: this tab's agent only, opening upward and fully inside the chat column.
    await win.getByTestId('model-chip').click()
    const menu = win.getByTestId('model-menu')
    await expect(menu).toBeVisible()
    await expect(menu.getByRole('menuitemradio', { name: /Opus/ }).first()).toBeVisible()
    await expect(menu.getByText('GPT-5')).toHaveCount(0)
    const mb = (await menu.boundingBox())!
    const aside = (await win.locator('aside').boundingBox())!
    expect(mb.x).toBeGreaterThanOrEqual(aside.x)
    expect(mb.x + mb.width).toBeLessThanOrEqual(aside.x + aside.width)
    await shotPanel(win, `chat-model-menu-${TAG}.png`)
    await win.keyboard.press('Escape')

    // Session tabs: a second tab is its own conversation; the first keeps its own.
    const tabs = win.getByTestId('chat-tab')
    await expect(tabs).toHaveCount(1)
    await expect(tabs.first()).toContainText('로그인 API')
    // "+" asks which agent the new tab runs; the model is picked inside the session.
    await win.getByTestId('chat-tab-new').click()
    await win.waitForTimeout(300)
    await shotPanel(win, `chat-new-tab-picker-${TAG}.png`)
    await expect(win.getByRole('button', { name: /^Sonnet|^Opus/ })).toHaveCount(0)
    await win.getByTestId('new-tab-agent-claude-code').click()
    await expect(tabs).toHaveCount(2)
    const visibleMsgs = win.locator('[role=tabpanel]:visible [data-testid=chat-message]')
    await expect(visibleMsgs).toHaveCount(0)
    await sendAndWait(win, '두 번째 세션 인사')
    await expect(visibleMsgs.last()).toContainText('Hello from the fake ACP agent.')
    await expect(tabs.nth(1)).toContainText('두 번째 세션')
    await shotPanel(win, `chat-tabs-${TAG}${CHAT_W === '300' ? '-narrow' : ''}.png`)
    await tabs.first().click()
    await expect(visibleMsgs.first()).toContainText('로그인 API')
    await tabs.nth(1).getByRole('button', { name: '세션 닫기 (에이전트 종료)' }).click()
    await expect(tabs).toHaveCount(1)
  } finally {
    await app.close()
    fs.rmSync(userData, { recursive: true, force: true })
  }
})
