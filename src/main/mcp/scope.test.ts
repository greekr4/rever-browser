import { afterEach, describe, expect, it } from 'vitest'

import {
  addScopeHosts,
  clearScope,
  getScope,
  hostInScope,
  hostnameOf,
  registrableDomain,
  scopeBlockForUrl,
  setScope
} from './scope'

afterEach(() => clearScope())

describe('scope', () => {
  describe('hostnameOf', () => {
    it('url·bare host·와일드카드·포트를 모두 호스트명으로 정규화한다', () => {
      expect(hostnameOf('https://api.example.com/v3/order')).toBe('api.example.com')
      expect(hostnameOf('Example.COM')).toBe('example.com')
      expect(hostnameOf('*.example.com')).toBe('example.com')
      expect(hostnameOf('x.io:8443/path')).toBe('x.io')
      expect(hostnameOf('')).toBeNull()
    })
  })

  describe('registrableDomain', () => {
    it('서브도메인을 등록가능도메인(eTLD+1)으로 접는다', () => {
      expect(registrableDomain('api.example.com')).toBe('example.com')
      expect(registrableDomain('a.b.c.example.com')).toBe('example.com')
      expect(registrableDomain('example.com')).toBe('example.com')
    })
    it('2단계 ccTLD를 처리한다', () => {
      expect(registrableDomain('shop.example.co.kr')).toBe('example.co.kr')
      expect(registrableDomain('www.example.co.uk')).toBe('example.co.uk')
    })
  })

  describe('hostInScope', () => {
    it('빈 허용목록은 모든 호스트를 통과시킨다', () => {
      expect(hostInScope('anything.com', [])).toBe(true)
    })
    it('같은 등록가능도메인의 서브도메인은 허용된다', () => {
      expect(hostInScope('api.example.com', ['example.com'])).toBe(true)
      expect(hostInScope('cdn.example.com', ['example.com'])).toBe(true)
    })
    it('다른 도메인은 거부된다', () => {
      expect(hostInScope('evil.com', ['example.com'])).toBe(false)
      expect(hostInScope('ads.doubleclick.net', ['example.com'])).toBe(false)
    })
  })

  describe('setScope/getScope', () => {
    it('입력을 등록가능도메인으로 저장하고 중복을 제거한다', () => {
      setScope(['https://www.example.com/x', 'api.example.com', 'other.io'])
      expect(getScope()).toEqual({ domains: ['example.com', 'other.io'], enabled: true })
    })
  })

  describe('scopeBlockForUrl', () => {
    it('스코프 미설정이면 차단하지 않는다', () => {
      expect(scopeBlockForUrl('https://anything.com/a')).toBeNull()
    })
    it('스코프 내 호스트는 통과, 밖은 메시지를 반환한다', () => {
      setScope(['example.com'])
      expect(scopeBlockForUrl('https://api.example.com/v3')).toBeNull()
      const blocked = scopeBlockForUrl('https://evil.com/steal')
      expect(blocked).toContain('Out of scope')
      expect(blocked).toContain('evil.com')
    })
    it('addScopeHosts로 추가하면 통과한다', () => {
      setScope(['example.com'])
      addScopeHosts(['https://partner.io'])
      expect(scopeBlockForUrl('https://partner.io/cb')).toBeNull()
    })
    it('파싱 불가 url은 차단하지 않는다(오탐 방지)', () => {
      setScope(['example.com'])
      expect(scopeBlockForUrl('not a url')).toBeNull()
      expect(scopeBlockForUrl(undefined)).toBeNull()
    })
  })
})
