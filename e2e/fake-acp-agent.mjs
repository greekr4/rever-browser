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

    await say('Hello from the fake ACP agent.')
    await flush()
    return { stopReason: 'end_turn' }
  }
}

const input = Writable.toWeb(process.stdout)
const output = Readable.toWeb(process.stdin)
const stream = acp.ndJsonStream(input, output)
new acp.AgentSideConnection((conn) => new FakeAgent(conn), stream)
