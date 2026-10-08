import path from 'node:path'

import { expect, test, usedTool, ROOT } from './fixtures'

// import_har through the chat: hand the agent a HAR from elsewhere and see
// that it imports it and reads the auth scheme off the offline entries.
test.describe('import_har (채팅)', () => {
  test.beforeEach(async ({ rever }) => {
    await rever.newChat()
  })

  test('HAR 파일을 가져와 인증·서명 방식을 설명한다', async ({ rever }) => {
    const har = path.join(ROOT, 'test-fixtures/har/shop-api.har')

    const turn = await rever.chat(
      `${har} 파일은 다른 브라우저에서 저장한 HAR야. 불러와서 이 API의 인증 방식과 요청 서명 헤더를 설명해줘. 401이 난 요청은 왜 실패했는지도.`
    )

    expect(usedTool(turn, 'import_har'), `tools: ${turn.tools.join(', ')}`).toBe(true)
    expect(turn.text).toMatch(/x-signature/i)
    expect(turn.text).toMatch(/Bearer/i)
  })
})
