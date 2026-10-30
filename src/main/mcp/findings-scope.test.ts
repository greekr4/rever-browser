import { describe, it, expect } from 'vitest'

import { originOf, filterByOrigin } from './findings-scope'

describe('originOf', () => {
  it('http(s) URL을 등록가능도메인으로 접는다', () => {
    expect(originOf('https://api.careernote.io/v1/x')).toBe('careernote.io')
    expect(originOf('https://www.example.co.kr/')).toBe('example.co.kr')
  })
  it('localhost는 포트를 유지해 앱별로 구분한다', () => {
    expect(originOf('http://localhost:8080/my')).toBe('localhost:8080')
    expect(originOf('http://localhost:3000/v1/jobs')).toBe('localhost:3000')
  })
  it('http(s)가 아니거나 못 읽으면 undefined', () => {
    expect(originOf('about:blank')).toBeUndefined()
    expect(originOf('')).toBeUndefined()
    expect(originOf(undefined)).toBeUndefined()
  })
})

describe('filterByOrigin', () => {
  const list = [
    { id: '1', origin: 'localhost' },
    { id: '2', origin: 'careernote.io' },
    { id: '3' } // 옛 발견 — origin 없음
  ]
  it('all=true면 origin 없는 옛 발견까지 전부', () => {
    expect(filterByOrigin(list, 'localhost', true).map((f) => f.id)).toEqual(['1', '2', '3'])
  })
  it('스코프를 주면 그 사이트 것만, 옛 발견과 다른 사이트는 뺀다', () => {
    expect(filterByOrigin(list, 'localhost', false).map((f) => f.id)).toEqual(['1'])
  })
  it('스코프가 undefined면(사이트 못 정함) 전부 반환 — 빈 보고서보다 낫다', () => {
    expect(filterByOrigin(list, undefined, false).map((f) => f.id)).toEqual(['1', '2', '3'])
  })
})
