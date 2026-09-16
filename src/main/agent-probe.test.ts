import { describe, expect, it } from 'vitest'

import { failureDetail } from './agent-probe'

describe('failureDetail', () => {
  it('원인이 없으면 기본 메시지만 반환한다', () => {
    expect(failureDetail('ACP connection closed', null, '')).toBe('ACP connection closed')
  })

  it('종료 코드와 stderr 꼬리를 붙여 실제 원인을 드러낸다', () => {
    const stderr = "'node' is not recognized as an internal or external command,\noperable program or batch file.\n"
    expect(failureDetail('ACP connection closed', 'exit 1', stderr)).toBe(
      "ACP connection closed — exit 1 — 'node' is not recognized as an internal or external command, operable program or batch file."
    )
  })

  it('stderr는 마지막 두 줄만, 200자로 제한한다', () => {
    const stderr = ['line one', 'line two', 'line three', 'line four'].join('\n')
    expect(failureDetail('boom', null, stderr)).toBe('boom — line three line four')

    const long = 'x'.repeat(500)
    expect(failureDetail('boom', null, long).length).toBeLessThanOrEqual('boom — '.length + 200)
  })

  it('빈 값은 구분자 없이 걸러낸다', () => {
    expect(failureDetail('boom', null, '   \n  \n')).toBe('boom')
    expect(failureDetail('boom', 'exit 2', '')).toBe('boom — exit 2')
  })
})
