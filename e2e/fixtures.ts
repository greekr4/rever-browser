import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { test as base, _electron as electron, type ElectronApplication, type Page } from '@playwright/test'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')

export interface ChatTurn {
  /** Visible text of the assistant's reply (tool groups included as labels). */
  text: string
  /** Tool titles called during the turn, e.g. "mcp__rever-traffic__inspect_listeners". */
  tools: string[]
  /** Tools that raised the approval prompt during the turn. */
  prompts: string[]
}

export interface ChatOptions {
  /** How to answer approval prompts (default: allow once). */
  permission?: 'allow' | 'reject'
  timeoutMs?: number
}

export function usedTool(turn: ChatTurn, name: string): boolean {
  return turn.tools.some((t) => t === name || t.endsWith(`__${name}`))
}

export class ReverApp {
  constructor(
    readonly app: ElectronApplication,
    readonly win: Page,
    private readonly mcpClient: Client
  ) {}

  /** Call an MCP tool directly — for deterministic setup, not for the behavior under test. */
  async mcp(name: string, args: Record<string, unknown> = {}): Promise<string> {
    const res = await this.mcpClient.callTool({ name, arguments: args })
    const content = (res.content ?? []) as Array<{ type: string; text?: string }>
    return content
      .filter((c) => c.type === 'text')
      .map((c) => c.text ?? '')
      .join('\n')
  }

  async navigate(url: string): Promise<void> {
    await this.mcp('browser_navigate', { url })
  }

  /** Start a fresh conversation (new agent session). */
  async newChat(): Promise<void> {
    const button = this.win.getByTestId('chat-new')
    // Disabled while the conversation is empty — already fresh.
    if (await button.isDisabled()) return
    await button.click()
    await this.win.getByTestId('chat-input').waitFor({ state: 'visible' })
  }

  /** Send one message through the chat panel and wait for the turn to finish. */
  async chat(text: string, opts: ChatOptions = {}): Promise<ChatTurn> {
    const { permission = 'allow', timeoutMs = 5 * 60_000 } = opts
    const form = this.win.getByTestId('chat-form')
    const assistant = this.win.locator('[data-testid=chat-message][data-role=assistant]')
    const before = await assistant.count()

    await this.win.getByTestId('chat-input').fill(text)
    await this.win.getByTestId('chat-input').press('Enter')

    const prompts: string[] = []
    const deadline = Date.now() + timeoutMs
    let started = false
    for (;;) {
      if (Date.now() > deadline) throw new Error(`chat turn timed out after ${timeoutMs}ms: ${text}`)
      const prompt = this.win.getByTestId('permission-prompt')
      if (await prompt.isVisible().catch(() => false)) {
        prompts.push((await prompt.getAttribute('data-tool')) ?? '?')
        const kind = permission === 'allow' ? 'perm-allow_once' : 'perm-reject_once'
        await this.win.getByTestId(kind).click()
        continue
      }
      const status = await form.getAttribute('data-status')
      if (status === 'submitted' || status === 'streaming') started = true
      if (started && (status === 'ready' || status === 'error')) break
      await this.win.waitForTimeout(500)
    }

    const msg = assistant.nth(before)
    const tools: string[] = []
    for (const group of await msg.getByTestId('chat-work').all()) {
      tools.push(...(JSON.parse((await group.getAttribute('data-tools')) ?? '[]') as string[]))
    }
    return { text: await msg.innerText(), tools, prompts }
  }
}

async function mainWindow(app: ElectronApplication): Promise<Page> {
  for (let i = 0; i < 60; i++) {
    for (const w of app.windows()) {
      if ((await w.getByTestId('chat-input').count().catch(() => 0)) > 0) return w
    }
    await new Promise((r) => setTimeout(r, 500))
  }
  throw new Error('main window with the chat panel never appeared')
}

async function connectMcp(userData: string): Promise<Client> {
  const file = path.join(userData, 'mcp-endpoint.json')
  for (let i = 0; i < 60 && !fs.existsSync(file); i++) await new Promise((r) => setTimeout(r, 500))
  const { url, token } = JSON.parse(fs.readFileSync(file, 'utf8')) as { url: string; token?: string }
  const client = new Client({ name: 'rever-e2e', version: '1' })
  await client.connect(
    new StreamableHTTPClientTransport(new URL(url), {
      requestInit: { headers: token ? { Authorization: `Bearer ${token}` } : {} }
    })
  )
  return client
}

// One app per test file: launching Electron and spawning the agent is slow.
export const test = base.extend<object, { rever: ReverApp }>({
  rever: [
    async ({}, use) => {
      // Isolated profile: no chat history, cookies or settings from the real one.
      const userData = fs.mkdtempSync(path.join(os.tmpdir(), 'rever-e2e-'))
      const app = await electron.launch({
        args: ['.'],
        cwd: ROOT,
        env: { ...process.env, REVER_USER_DATA_DIR: userData }
      })
      try {
        let win = await mainWindow(app)
        // Skip first-run onboarding; the default agent (Claude Code) is used.
        await win.evaluate(() => localStorage.setItem('rev:agent-onboarded', 'skipped'))
        await win.reload()
        win = await mainWindow(app)
        const mcpClient = await connectMcp(userData)
        await use(new ReverApp(app, win, mcpClient))
        await mcpClient.close()
      } finally {
        await app.close()
        fs.rmSync(userData, { recursive: true, force: true })
      }
    },
    { scope: 'worker' }
  ]
})

export { expect } from '@playwright/test'
