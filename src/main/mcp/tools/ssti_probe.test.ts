import { describe, expect, it } from 'vitest'

import { buildSstiCases, sstiEvaluated } from './ssti_probe'

describe('ssti_probe helpers', () => {
  it('엔진별 페이로드를 만들고 기대 결과는 산술 곱을 nonce 로 감싼다', () => {
    const cases = buildSstiCases(101, 103, 'zABCz')
    const prod = 101 * 103 // 10403
    expect(cases.length).toBeGreaterThanOrEqual(6)
    for (const c of cases) {
      expect(c.expected).toBe(`zABCz${prod}zABCz`)
      // 페이로드에는 산술식이 리터럴로 들어 있고, 아직 계산 결과는 없다
      expect(c.payload).toContain('101')
      expect(c.payload).toContain('103')
      expect(c.payload).not.toContain(String(prod))
    }
    expect(cases.map((c) => c.engine)).toContain('velocity')
    expect(cases.map((c) => c.engine)).toContain('jinja2/twig')
  })

  it('계산 결과(평가됨)가 응답에 있으면 confirmed, 리터럴만 있으면 아니다', () => {
    const [generic] = buildSstiCases(101, 103, 'zABCz')
    // 엔진이 평가한 경우: nonce + 10403 + nonce 가 응답에 나타난다
    expect(sstiEvaluated('<h2>zABCz10403zABCz</h2>', generic)).toBe(true)
    // 평가 안 된 경우: 페이로드가 리터럴로 반사(계산 안 됨)
    expect(sstiEvaluated(`<h2>${generic.payload}</h2>`, generic)).toBe(false)
    // 무관한 본문
    expect(sstiEvaluated('<h2>Example text</h2>', generic)).toBe(false)
  })

  it('velocity 페이로드는 #set 과 nonce 로 감싼 참조를 쓴다', () => {
    const velocity = buildSstiCases(101, 103, 'zABCz').find((c) => c.engine === 'velocity')!
    expect(velocity.payload).toContain('#set($q=101*103)')
    expect(velocity.payload).toContain('zABCz${q}zABCz')
  })
})
