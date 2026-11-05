import { describe, it, expect } from 'vitest'

import { buildSendExpression } from './ws-capture'

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
