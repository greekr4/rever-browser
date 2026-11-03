import { describe, expect, it } from 'vitest'

import { buildCmdCases, cmdEvaluated, urlSlotToMarker } from './cmd_probe'

describe('buildCmdCases', () => {
  it('탈출 컨텍스트별 페이로드를 만들고 기대 결과는 산술 곱을 nonce 로 감싼다', () => {
    const cases = buildCmdCases(101, 103, 'zXYZz')
    const prod = 101 * 103 // 10403
    expect(cases.map((c) => c.context)).toEqual([
      'single-quote',
      'double-quote',
      'semicolon',
      'pipe',
      'backtick',
      'subshell'
    ])
    for (const c of cases) {
      expect(c.expected).toBe(`zXYZz${prod}zXYZz`)
      // 셸이 평가할 산술식이 리터럴로 들어 있고, 결과값은 없다
      expect(c.payload).toContain('$(( 101 * 103 ))')
      expect(c.payload).not.toContain(String(prod))
    }
  })

  it('셸이 실행한 경우(계산 결과 등장)만 confirmed, 리터럴 반사는 아니다', () => {
    const [sq] = buildCmdCases(101, 103, 'zXYZz')
    expect(cmdEvaluated('<h1>zXYZz10403zXYZz</h1>', sq)).toBe(true)
    expect(cmdEvaluated(`<h1>${sq.payload}</h1>`, sq)).toBe(false)
    expect(cmdEvaluated("error: sh: 1: Syntax error: '' unmatched", sq)).toBe(false)
  })
})

describe('urlSlotToMarker', () => {
  it('기존 쿼리 파라미터 값을 § 마커로 바꾼다', () => {
    expect(urlSlotToMarker('http://x/?format=%Y-%m-%d&b=2', 'format')).toBe('http://x/?format=§&b=2')
    expect(urlSlotToMarker('http://x/?a=1&fmt=1', 'fmt')).toBe('http://x/?a=1&fmt=§')
  })
  it('파라미터가 없으면 추가한다', () => {
    expect(urlSlotToMarker('http://x/', 'format')).toBe('http://x/?format=§')
    expect(urlSlotToMarker('http://x/?a=1', 'format')).toBe('http://x/?a=1&format=§')
  })
  it('파라미터명의 정규식 특수문자는 이스케이프된다', () => {
    expect(urlSlotToMarker('http://x/?f.mt=1', 'f.mt')).toBe('http://x/?f.mt=§')
  })
})
