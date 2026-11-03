import { describe, it, expect } from 'vitest'

import { originOf, filterByOrigin, resolveExportScope } from './findings-scope'

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

describe('resolveExportScope', () => {
  it('명시적 site가 활성 탭보다 우선한다 — 탭이 없어도 스코프가 잡힌다', () => {
    expect(resolveExportScope('http://localhost:1337/', undefined, false)).toEqual({ scope: 'localhost:1337' })
    expect(resolveExportScope('https://arqoven.com', 'http://other:8080/', false)).toEqual({ scope: 'arqoven.com' })
  })
  it('site가 없으면 활성 탭 URL로 스코프를 잡는다', () => {
    expect(resolveExportScope(undefined, 'https://api.careernote.io/v1/x', false)).toEqual({ scope: 'careernote.io' })
  })
  it('둘 다 없으면 전체 덤프 대신 안내 에러를 낸다 (343KB 사고 회귀 방지)', () => {
    const r = resolveExportScope(undefined, undefined, false)
    expect(r.scope).toBeUndefined()
    expect(r.error).toContain('site')
  })
  it('all=true면 에러 없이 전체 내보내기 허용', () => {
    expect(resolveExportScope(undefined, undefined, true)).toEqual({})
  })
})
