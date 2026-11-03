import { describe, expect, it } from 'vitest'

import { buildSstiCases, sstiEvaluated, formSlotToMarker } from './ssti_probe'

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

describe('formSlotToMarker', () => {
  it('기존 폼 필드 값을 § 마커로 바꾼다', () => {
    expect(formSlotToMarker('text=hello+world', 'text')).toBe('text=§')
    expect(formSlotToMarker('a=1&text=%7B%7B7%2A7%7D%7D&b=2', 'text')).toBe('a=1&text=§&b=2')
  })
  it('필드가 없으면 추가한다 — 마커 스테이징 없이 param 만으로 주입 가능', () => {
    expect(formSlotToMarker('', 'text')).toBe('text=§')
    expect(formSlotToMarker('a=1', 'text')).toBe('a=1&text=§')
  })
  it('파라미터명의 정규식 특수문자는 이스케이프된다', () => {
    expect(formSlotToMarker('q.x=1', 'q.x')).toBe('q.x=§')
  })
  it('빈 형제 필드는 더미 값으로 채운다 — KORP Terminal 회귀 (크롤이 빈 폼 제출)', () => {
    // 크롤이 username=&password= (빈 값) 을 캡처 → password 가 비어 있으면
    // 앱이 400 Missing parameters 로 주입 전에 차단한다
    expect(formSlotToMarker('username=&password=', 'username')).toBe('username=§&password=reverprobe')
    // 연속된 빈 필드도 모두 채운다 (구분자 lookahead)
    expect(formSlotToMarker('a=&b=&c=1', 'c')).toBe('a=reverprobe&b=reverprobe&c=§')
    // 값 있는 형제 필드는 그대로 둔다
    expect(formSlotToMarker('user=admin&pass=', 'user')).toBe('user=§&pass=reverprobe')
  })
})
