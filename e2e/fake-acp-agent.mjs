// A minimal ACP agent used only by e2e (gated behind REVER_FAKE_ACP_AGENT in
// src/main/ipc/acp-ipc.ts). It never calls a model: it inspects the prompt for
// an [[E2E:...]] marker and replays a deterministic turn so the chat UI's
// handling of provider safety events can be asserted and screenshotted without
// trying to trip a real safeguard.
import * as acp from '@agentclientprotocol/sdk'
import { Readable, Writable } from 'node:stream'

const FALLBACK_NOTICE =
  'Model fallback: claude-fable-5-1 declined this request (cyber); retried with ' +
  'claude-opus-4-8. The session will continue on claude-opus-4-8.'

class FakeAgent {
  constructor(connection) {
    this.connection = connection
  }

  async initialize() {
    return { protocolVersion: acp.PROTOCOL_VERSION, agentCapabilities: { loadSession: false } }
  }

  async newSession() {
    const sessionId = Array.from(crypto.getRandomValues(new Uint8Array(16)))
      .map((b) => b.toString(16).padStart(2, '0'))
      .join('')
    return { sessionId }
  }

  async authenticate() {
    return {}
  }

  async setSessionMode() {
    return {}
  }

  async cancel() {}

  async prompt(params) {
    const text = (params.prompt ?? []).map((p) => p.text ?? '').join('\n')
    const say = (t) =>
      this.connection.sessionUpdate({
        sessionId: params.sessionId,
        update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: t } }
      })
    // Let the last update's IPC reach the renderer before end_turn resolves the
    // prompt — a real agent always has model latency here; at zero latency the
    // final chunk races the turn's finish and can be dropped.
    const flush = () => new Promise((r) => setTimeout(r, 50))

    // Hard block: nothing runs, turn ends with the 'refusal' stop reason.
    if (text.includes('[[E2E:REFUSE]]')) {
      return { stopReason: 'refusal' }
    }

    // Soft flag + model fallback: the real SDK streams the notice as its own
    // chunk glued to the answer with no surrounding newlines (run-on). Reproduce
    // that so mapUpdate's paragraph isolation is exercised.
    if (text.includes('[[E2E:FALLBACK]]')) {
      await say(FALLBACK_NOTICE)
      await say('Here is what I found: the login endpoint is POST /api/auth.')
      await flush()
      return { stopReason: 'end_turn' }
    }

    // Mixed Korean markdown, for typography screenshots.
    if (text.includes('[[E2E:RICH]]')) {
      await say(RICH_ANSWER)
      await flush()
      return { stopReason: 'end_turn' }
    }

    // A turn with reasoning + one tool call, for screenshots of the work block.
    if (text.includes('[[E2E:TOOLS]]')) {
      const up = (update) => this.connection.sessionUpdate({ sessionId: params.sessionId, update })
      await up({ sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text: 'Check the captured traffic first.' } })
      await up({ sessionUpdate: 'tool_call', toolCallId: 't1', title: 'mcp__rever-traffic__list_requests', rawInput: { limit: 20 } })
      await up({ sessionUpdate: 'tool_call_update', toolCallId: 't1', status: 'completed', rawOutput: '[{"method":"POST","url":"/api/v3/login"}]' })
      await say('The login call is **POST /api/v3/login**. It returns an HS256 JWT stored in `localStorage["auth.token"]`.')
      await flush()
      return { stopReason: 'end_turn' }
    }

    await say('Hello from the fake ACP agent.')
    await flush()
    return { stopReason: 'end_turn' }
  }
}

const RICH_ANSWER = `## 로그인 흐름 분석

\`POST /api/v3/login\` 요청이 HS256 JWT를 돌려주고, 클라이언트는 이를 \`localStorage["auth.token"]\`에 저장합니다. 이후 모든 API 호출에는 **x-signature** 헤더가 붙습니다.

### 서명 방식
- 알고리즘: HMAC-SHA256
- 입력: \`METHOD\\nPATH\\nBODY\\nTIMESTAMP\`
- 키: 번들에 하드코딩됨 (\`s3cr3t-signing-key\`)

| 엔드포인트 | 메서드 | 인증 | 비고 |
|---|---|---|---|
| /api/v3/login | POST | 없음 | JWT 발급 |
| /api/v3/items | GET | 서명 | 페이지네이션 |
| /api/v3/admin/keys | GET | 서명 | 페이지 코드에 없음 |

\`\`\`js
const sig = await hmac(key, \`\${method}\\n\${path}\\n\${body}\\n\${ts}\`)
headers['x-signature'] = sig
\`\`\`

> 서명 키가 클라이언트에 있으므로 재전송 시 직접 다시 서명할 수 있습니다.

다음 단계로 \`/api/v3/admin/keys\`에 서명된 요청을 보내 권한 검사가 있는지 확인해 보는 것을 권합니다.`

const input = Writable.toWeb(process.stdout)
const output = Readable.toWeb(process.stdin)
const stream = acp.ndJsonStream(input, output)
new acp.AgentSideConnection((conn) => new FakeAgent(conn), stream)
