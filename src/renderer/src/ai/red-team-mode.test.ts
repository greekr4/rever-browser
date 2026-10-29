import { afterEach, describe, expect, it } from 'vitest'

import { hostInRedTeamScope, isRedTeamActive, setRedTeamScan } from './red-team-mode'

afterEach(() => setRedTeamScan(false))

describe('red-team-mode', () => {
  it('비활성일 때는 어떤 호스트도 범위 밖이다', () => {
    expect(isRedTeamActive()).toBe(false)
    expect(hostInRedTeamScope('calc.assasup.com')).toBe(false)
  })

  it('스캔 대상과 같은 eTLD+1 호스트만 범위 안이다', () => {
    setRedTeamScan(true, 'https://www.assasup.com/my')
    expect(isRedTeamActive()).toBe(true)
    expect(hostInRedTeamScope('assasup.com')).toBe(true)
    expect(hostInRedTeamScope('api.assasup.com')).toBe(true)
    expect(hostInRedTeamScope('evil.com')).toBe(false)
    expect(hostInRedTeamScope(null)).toBe(false)
  })

  it('localhost 는 포트까지 일치해야 범위 안이다 (8080 ≠ 8090)', () => {
    setRedTeamScan(true, 'http://localhost:8080/my')
    expect(hostInRedTeamScope('localhost:8080')).toBe(true)
    expect(hostInRedTeamScope('localhost:8090')).toBe(false)
    expect(hostInRedTeamScope('localhost')).toBe(false)
    expect(hostInRedTeamScope('example.com')).toBe(false)
  })

  it('중지하면 다시 범위 밖이다', () => {
    setRedTeamScan(true, 'https://assasup.com/')
    setRedTeamScan(false)
    expect(isRedTeamActive()).toBe(false)
    expect(hostInRedTeamScope('assasup.com')).toBe(false)
  })
})
