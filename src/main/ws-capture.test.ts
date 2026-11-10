import { describe, it, expect } from 'vitest'

import { buildSendExpression, handshakeInfo, HEAP_LIST_FN, HEAP_SEND_FN, RECONNECT_FN } from './ws-capture'

function evalResult(expr: string, captures: Array<{ readyState: number }>): string {
  // 표현식이 페이지에서 반환할 값을 시뮬레이션한다.
  const m = expr.match(/\(window\.__revWSCapture \|\| \[\]\)\[(\d+)\]/)
  const e = captures[Number(m?.[1] ?? -1)]
  if (!e) return 'no-such-capture'
  if (e.readyState !== 1) return 'not-open:' + e.readyState
  return 'ok'
}

describe('buildSendExpression', () => {
  it('열린 소켓이면 ok를 반환하는 표현식을 만든다 (텍스트)', () => {
    const payload = '{"move":[1,2]}'
    const expr = buildSendExpression(0, payload, false)
    expect(expr).toContain('e.ws.send(')
    expect(expr).toContain(JSON.stringify(payload))
    expect(evalResult(expr, [{ readyState: 1 }])).toBe('ok')
  })

  it('페이로드의 따옴표/특수문자는 JSON.stringify로 이스케이프된다', () => {
    const expr = buildSendExpression(1, 'say "hi" \\ & newline\n', false)
    expect(expr).toContain(JSON.stringify('say "hi" \\ & newline\n'))
    expect(evalResult(expr, [{ readyState: 1 }, { readyState: 1 }])).toBe('ok')
  })

  it('바이너리 경로는 base64 → Uint8Array 변환을 포함한다', () => {
    const expr = buildSendExpression(0, 'AAECAwQ=', true)
    expect(expr).toContain('atob("AAECAwQ=")')
    expect(expr).toContain('Uint8Array')
  })

  it('닫힌 소켓이면 not-open을 반환하는 표현식이다', () => {
    const expr = buildSendExpression(0, 'x', false)
    expect(evalResult(expr, [{ readyState: 3 }])).toBe('not-open:3')
  })

  it('없는 captureId면 no-such-capture를 반환하는 표현식이다', () => {
    const expr = buildSendExpression(5, 'x', false)
    expect(evalResult(expr, [{ readyState: 1 }])).toBe('no-such-capture')
  })
})

describe('heap socket functions (Runtime.queryObjects 결과에 대해 실행)', () => {
  type Fake = { url: string; readyState: number; sent: unknown[]; send: (d: unknown) => void }
  const sock = (url: string, readyState = 1): Fake => {
    const f: Fake = { url, readyState, sent: [], send: (d) => f.sent.push(d) }
    return f
  }
  // Same shape CDP gives: `this` is the array of live sockets.
  const run = (fn: string, sockets: Fake[], ...args: unknown[]): unknown =>
    (new Function(`return (${fn})`)() as (...a: unknown[]) => unknown).apply(sockets, args)

  it('URL이 같은 열린 소켓 중 가장 최근 것으로 보낸다', () => {
    const old = sock('ws://a/ws?room=lobby')
    const fresh = sock('ws://a/ws?room=lobby')
    const other = sock('ws://a/ws?widget=chat')
    expect(run(HEAP_SEND_FN, [old, other, fresh], 'ws://a/ws?room=lobby', 'hi', false)).toBe('ok')
    expect(fresh.sent).toEqual(['hi'])
    expect(old.sent).toEqual([])
    expect(other.sent).toEqual([])
  })

  it('닫힌 소켓만 있으면 not-open, 없으면 no-socket', () => {
    expect(run(HEAP_SEND_FN, [sock('ws://a/ws', 3)], 'ws://a/ws', 'x', false)).toBe('not-open')
    expect(run(HEAP_SEND_FN, [sock('ws://b/ws')], 'ws://a/ws', 'x', false)).toBe('no-socket')
  })

  it('바이너리는 base64를 바이트로 풀어 보낸다', () => {
    const s = sock('ws://a/ws')
    expect(run(HEAP_SEND_FN, [s], 'ws://a/ws', btoa('\x01\x02'), true)).toBe('ok')
    expect(Array.from(s.sent[0] as Uint8Array)).toEqual([1, 2])
  })

  it('목록은 URL과 상태만 돌려준다', () => {
    expect(run(HEAP_LIST_FN, [sock('ws://a/ws', 1), sock('ws://b/ws', 0)])).toEqual([
      { url: 'ws://a/ws', readyState: 1 },
      { url: 'ws://b/ws', readyState: 0 }
    ])
  })
})

describe('handshakeInfo', () => {
  it('핸드셰이크 헤더에서 서브프로토콜 목록과 오리진을 뽑는다 (헤더 이름 대소문자 무시)', () => {
    expect(handshakeInfo({ 'Sec-WebSocket-Protocol': 'graphql-ws, v2.chat', Origin: 'http://localhost:8778' })).toEqual({
      protocols: ['graphql-ws', 'v2.chat'],
      origin: 'http://localhost:8778'
    })
    expect(handshakeInfo({ 'sec-websocket-protocol': 'mqtt' }).protocols).toEqual(['mqtt'])
  })

  it('헤더가 없으면 서브프로토콜 없이 연결한다', () => {
    expect(handshakeInfo(undefined)).toEqual({ protocols: [], origin: undefined })
  })
})

describe('RECONNECT_FN (페이지에서 새 소켓을 연다)', () => {
  class FakeWS {
    static last: FakeWS
    onopen?: () => void
    onclose?: (e: { code: number; reason: string }) => void
    args: unknown[]
    constructor(...args: unknown[]) {
      this.args = args
      FakeWS.last = this
    }
    close(): void {}
  }
  const run = (url: string, protocols: string[]): Promise<string> =>
    (new Function('WebSocket', `return (${RECONNECT_FN})`)(FakeWS) as (u: string, p: string[]) => Promise<string>)(url, protocols)

  it('소켓이 열리면 ok, 서브프로토콜은 생성자에 그대로 넘긴다', async () => {
    const p = run('ws://a/ws', ['graphql-ws'])
    expect(FakeWS.last.args).toEqual(['ws://a/ws', ['graphql-ws']])
    FakeWS.last.onopen!()
    expect(await p).toBe('ok')
  })

  it('서브프로토콜이 없으면 URL만 넘긴다', async () => {
    const p = run('ws://a/ws', [])
    expect(FakeWS.last.args).toEqual(['ws://a/ws'])
    FakeWS.last.onopen!()
    await p
  })

  it('열리기 전에 닫히면 close code를 담은 사유를 돌려준다', async () => {
    const p = run('ws://a/ws', [])
    FakeWS.last.onclose!({ code: 1006, reason: '' })
    expect(await p).toContain('close code 1006')
  })
})
