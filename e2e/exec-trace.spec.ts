import { expect, test, usedTool } from './fixtures'

// exec_trace through the chat: given "find the signing code", does the agent
// record a window around the click and land on the signing module?
test.describe('exec_trace (채팅)', () => {
  test.beforeEach(async ({ rever }) => {
    await rever.newChat()
  })

  test('Get items 클릭 동안 실행된 코드에서 서명 함수를 찾는다', async ({ rever }) => {
    await rever.navigate('http://localhost:8779/')

    const turn = await rever.chat(
      'Get items 버튼을 누를 때 요청에 붙는 서명을 만드는 코드를 찾아줘. 버튼을 눌러도 돼. 원본 파일과 함수 이름까지 알려줘.'
    )

    expect(usedTool(turn, 'exec_trace'), `tools: ${turn.tools.join(', ')}`).toBe(true)
    expect(turn.text).toMatch(/signing\.ts/)
    expect(turn.text).toMatch(/hmacSha256|signedHeaders/)
  })
})
