import { expect, test, usedTool } from './fixtures'

// inspect_listeners through the chat: does the agent pick the new tool, skip
// clicking, and follow the handler to its original source?
test.describe('inspect_listeners (채팅)', () => {
  test.beforeEach(async ({ rever }) => {
    await rever.newChat()
  })

  test('클릭하지 않고 Sign in 핸들러와 원본 위치를 찾는다', async ({ rever }) => {
    await rever.navigate('http://localhost:8779/')

    const turn = await rever.chat(
      'Sign in 버튼을 누르면 어떤 코드가 실행되는지 클릭하지 말고 알아내줘. 원본 소스 위치까지.'
    )

    expect(usedTool(turn, 'inspect_listeners'), `tools: ${turn.tools.join(', ')}`).toBe(true)
    expect(turn.tools.some((t) => /browser_click|click_selector/.test(t))).toBe(false)
    expect(usedTool(turn, 'resolve_source'), `tools: ${turn.tools.join(', ')}`).toBe(true)
    expect(turn.text).toMatch(/app\.ts/)
    expect(turn.text).toMatch(/\b44\b/)
  })

  test('React 사이트에서 루트에 위임된 클릭 리스너를 찾는다', async ({ rever }) => {
    await rever.navigate('https://react.dev/')

    const turn = await rever.chat(
      '현재 페이지의 첫 번째 button 요소의 클릭 리스너를 조사해줘. 버튼 자체에 없으면 어디서 처리되는지도 알려줘.'
    )

    expect(usedTool(turn, 'inspect_listeners'), `tools: ${turn.tools.join(', ')}`).toBe(true)
    expect(turn.text).toMatch(/__next/)
  })
})
