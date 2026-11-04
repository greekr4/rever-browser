import { describe, expect, it, beforeEach } from 'vitest'

import { setAltSession, getAltSession, clearAltSession } from './redteam-tenants'

describe('redteam-tenants alt session', () => {
  beforeEach(() => clearAltSession())

  it('저장한 쿠키를 돌려준다', () => {
    setAltSession('ats_token=B; other=1')
    expect(getAltSession()).toBe('ats_token=B; other=1')
  })

  it('공백/빈 값은 null 로 지운다', () => {
    setAltSession('ats_token=B')
    setAltSession('   ')
    expect(getAltSession()).toBeNull()
  })

  it('clear 로 비운다', () => {
    setAltSession('x=1')
    clearAltSession()
    expect(getAltSession()).toBeNull()
  })
})
