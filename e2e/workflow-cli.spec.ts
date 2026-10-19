import { expect, test } from './fixtures'

interface ListedRequest {
  requestId: string
  method: string
  url: string
}

interface CompiledCli {
  id: string
  name: string
  writes: boolean
  commands: Array<{ name: string; method: string; path: string; effect: string }>
}

const FIXTURE = 'http://127.0.0.1:8779/'

async function waitForRequest(
  rever: { mcp: (name: string, args?: Record<string, unknown>) => Promise<string>; win: { waitForTimeout: (ms: number) => Promise<void> } },
  pred: (row: ListedRequest) => boolean
): Promise<ListedRequest> {
  const started = Date.now()
  let last = ''
  for (;;) {
    last = await rever.mcp('list_requests', { limit: 100 })
    const rows = JSON.parse(last) as ListedRequest[]
    const hit = rows.find(pred)
    if (hit) return hit
    if (Date.now() - started > 15_000) {
      throw new Error(`request not captured: ${last.slice(0, 800)}`)
    }
    await rever.win.waitForTimeout(250)
  }
}

test('고른 호출만 CLI가 되고 쓰기는 기본으로 막힌다', async ({ rever }) => {
  test.setTimeout(120_000)
  await rever.navigate(FIXTURE)
  await rever.mcp('cli_arm', { action: 'start' })

  const echoClick = await rever.mcp('browser_click_selector', { selector: '#btn-echo' })
  expect(echoClick).not.toMatch(/error|no active/i)
  const echo = await waitForRequest(
    rever,
    (row) => row.method === 'GET' && row.url.includes('/api/v3/echo')
  )

  const armed = await rever.mcp('cli_arm', { action: 'list' })
  expect(armed).toContain(echo.requestId)
  expect(armed).not.toContain('app.js')

  const compiledText = await rever.mcp('cli_compile', { requestIds: [echo.requestId] })
  const compiled = JSON.parse(compiledText) as CompiledCli
  expect(compiled.writes).toBe(false)
  expect(compiled.commands).toHaveLength(1)
  expect(compiled.commands[0]).toMatchObject({
    path: '/api/v3/echo',
    method: 'GET',
    effect: 'read'
  })
  expect(compiledText).not.toContain('app.js')

  const described = await rever.mcp('cli_describe', { id: compiled.id, command: compiled.commands[0].name })
  expect(described).toContain('--name')

  const ran = await rever.mcp('cli_run', {
    id: compiled.id,
    command: compiled.commands[0].name,
    inputs: { name: 'rever' }
  })
  expect(ran).toContain('rever')
  expect(ran.toLowerCase()).not.toContain('cookie')

  const loginClick = await rever.mcp('browser_click_selector', { selector: '#btn-login' })
  expect(loginClick).not.toMatch(/no active/i)
  const login = await waitForRequest(
    rever,
    (row) => row.method === 'POST' && row.url.includes('/api/v3/login')
  )
  const loginCli = JSON.parse(
    await rever.mcp('cli_compile', { requestIds: [login.requestId] })
  ) as CompiledCli
  expect(loginCli.commands[0]?.effect).toBe('write')
  const refused = await rever.mcp('cli_run', {
    id: loginCli.id,
    command: loginCli.commands[0].name,
    inputs: { user: 'u42', pass: 'nope' }
  })
  expect(refused).toBe('writes disabled for this CLI')

  const exported = await rever.mcp('cli_export', { id: loginCli.id })
  expect(exported).not.toContain('hunter2')
  expect(exported).not.toMatch(/Cookie:/i)
  expect(exported).toContain('$pass')

  const win = rever.win
  await win.getByTestId('chip-workflows').click()
  await expect(win.locator('[data-testid=workflow-row][data-name="post-api-v3-login"]')).toBeVisible()
  await win.locator('[data-testid=workflow-row][data-name="post-api-v3-login"]').getByTestId('workflow-edit').click()
  await win.getByTestId('cli-export').click()
  const savedExport = await win.getByTestId('cli-export-text').innerText()
  expect(savedExport).not.toContain('hunter2')
  expect(savedExport).not.toMatch(/Cookie:/i)

  await win.getByTestId('workflow-cancel').click()
  await win.getByTestId('workflow-new-cli').click()
  await expect(win.getByTestId('workflow-save')).toBeDisabled()
  const echoBox = win.getByTestId(`cli-request-${echo.requestId}`)
  await expect(echoBox).toBeVisible()
  await echoBox.locator('input').check()
  await win.getByTestId('cli-compile').click()
  await expect(win.locator('[data-testid=cli-command][data-name="get-api-v3-echo"]')).toBeVisible()
  await expect(win.getByTestId('workflow-save')).toBeEnabled()
  await win.getByTestId('cli-flag-name').fill('rever')
  const example = await win.getByTestId('cli-call-example').innerText()
  expect(example).toContain('"command": "get-api-v3-echo"')
  expect(example).toContain('"name": "rever"')
  expect(example).not.toMatch(/cookie/i)
  await win.getByTestId('cli-run').click()
  await expect(win.getByTestId('cli-run-result')).toContainText('rever')
  await win.getByTestId('cli-export').click()
  const uiExport = await win.getByTestId('cli-export-text').innerText()
  expect(uiExport).not.toContain('hunter2')
  expect(uiExport).not.toMatch(/Cookie:/i)
})
