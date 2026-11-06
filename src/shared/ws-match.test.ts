import { describe, it, expect } from 'vitest'

import { matchCaptureForUrl } from './ws-match'

const caps = [
  { id: 0, url: 'wss://game.example/ws/lobby?v=aaa', readyState: 1 },
  { id: 1, url: 'wss://game.example/ws/camp-1', readyState: 1 },
  { id: 2, url: 'wss://other.example/ws', readyState: 3 }
]

describe('matchCaptureForUrl', () => {
  it('URL이 정확히 같은 열린 소켓을 찾는다', () => {
    expect(matchCaptureForUrl(caps, 'wss://game.example/ws/camp-1')?.id).toBe(1)
  })

  it('쿼리스트링만 다르면 무시하고 매칭한다 (재연결 토큰 변경)', () => {
    expect(matchCaptureForUrl(caps, 'wss://game.example/ws/lobby?v=zzz')?.id).toBe(0)
  })

  it('닫힌 소켓은 매칭하지 않는다', () => {
    expect(matchCaptureForUrl(caps, 'wss://other.example/ws')).toBeNull()
  })

  it('매칭 없으면 null', () => {
    expect(matchCaptureForUrl(caps, 'wss://unknown.example/ws')).toBeNull()
  })
})
