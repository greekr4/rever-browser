import { describe, it, expect } from 'vitest'
import { mergeReplayHeaders } from './utils'

describe('mergeReplayHeaders', () => {
  it('대소문자가 달라도 override가 원본 헤더를 실제로 제거한다', () => {
    const h = mergeReplayHeaders({ authorization: 'Bearer real', accept: '*/*' }, '', {
      Authorization: ''
    })
    expect(h).toEqual({ accept: '*/*' })
  })

  it('override가 없으면 페이지 쿠키를 자동 주입한다', () => {
    const h = mergeReplayHeaders({ accept: '*/*' }, 'sid=1', undefined)
    expect(h['cookie']).toBe('sid=1')
  })

  it('명시적 cookie override가 자동 주입 쿠키를 이긴다 (인증 제거 재생)', () => {
    const h = mergeReplayHeaders({ cookie: 'old=1' }, 'sid=1', { cookie: '' })
    expect('cookie' in h).toBe(false)
  })

  it('일반 override는 값을 교체한다', () => {
    const h = mergeReplayHeaders({ 'x-token': 'a' }, '', { 'X-Token': 'b' })
    expect(h).toEqual({ 'x-token': 'b' })
  })
})
