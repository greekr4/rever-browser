import { describe, it, expect } from 'vitest'

import { filterTriggered, formatTriggeredRequests, outsideActions } from './triggered-requests'
import type { StoredRequest } from '../traffic-store'

function req(partial: Partial<StoredRequest> & { requestId: string }): StoredRequest {
  return {
    url: 'https://api.example.com/x',
    host: 'api.example.com',
    method: 'GET',
    resourceType: 'XHR',
    startedAt: 0,
    ...partial
  }
}

const log = (id: string) =>
  req({ requestId: id, method: 'POST', url: 'https://log.example.com/putlog/client/abc' })

describe('filterTriggered', () => {
  it('XHR/Fetch만 남기고 이미지·스크립트는 뺀다', () => {
    const { shown } = filterTriggered(
      [
        req({ requestId: '1', resourceType: 'Image' }),
        req({ requestId: '2', resourceType: 'Script' }),
        req({ requestId: '3', resourceType: 'Fetch', url: 'https://api.example.com/cart' })
      ],
      [],
      1
    )
    expect(shown.map((r) => r.requestId)).toEqual(['3'])
  })

  it('다른 탭 요청은 뺀다', () => {
    const { shown } = filterTriggered(
      [req({ requestId: '1', tabId: 1 }), req({ requestId: '2', tabId: 2 })],
      [],
      1
    )
    expect(shown.map((r) => r.requestId)).toEqual(['1'])
  })

  it('액션 직전에 두 번 이상 반복되던 엔드포인트는 숨기고 개수를 센다', () => {
    const { shown, hidden } = filterTriggered(
      [log('a'), req({ requestId: 'b', method: 'POST', url: 'https://api.example.com/cart' })],
      [log('p1'), log('p2')],
      1
    )
    expect(shown.map((r) => r.requestId)).toEqual(['b'])
    expect(hidden).toBe(1)
  })

  it('직전에 한 번만 나온 엔드포인트는 숨기지 않는다 (같은 버튼을 두 번 누른 경우)', () => {
    const cart = (id: string) => req({ requestId: id, method: 'POST', url: 'https://api.example.com/cart' })
    const { shown, hidden } = filterTriggered([cart('2')], [cart('1')], 1)
    expect(shown.map((r) => r.requestId)).toEqual(['2'])
    expect(hidden).toBe(0)
  })

  it('경로의 숫자 ID가 달라도 같은 엔드포인트로 본다', () => {
    const poll = (id: string, n: number) => req({ requestId: id, url: `https://api.example.com/poll/${n}` })
    const { shown, hidden } = filterTriggered([poll('c', 3)], [poll('a', 1), poll('b', 2)], 1)
    expect(shown).toEqual([])
    expect(hidden).toBe(1)
  })

  it('시작 시각 오름차순으로 돌려준다', () => {
    const { shown } = filterTriggered(
      [req({ requestId: 'late', startedAt: 20 }), req({ requestId: 'early', startedAt: 10 })],
      [],
      undefined
    )
    expect(shown.map((r) => r.requestId)).toEqual(['early', 'late'])
  })
})

describe('outsideActions', () => {
  it('이전 액션 구간에서 나간 요청은 배경 판정에서 뺀다 (같은 버튼 연타)', () => {
    const cart = (id: string, at: number) =>
      req({ requestId: id, method: 'POST', url: 'https://api.example.com/cart', startedAt: at })
    const background = outsideActions([cart('1', 100), cart('2', 300)], [[90, 150], [290, 350]])
    expect(background).toEqual([])
    const { shown } = filterTriggered([cart('3', 500)], background, 1)
    expect(shown.map((r) => r.requestId)).toEqual(['3'])
  })

  it('액션 구간 밖의 반복 요청은 그대로 배경으로 남긴다 (폴링)', () => {
    const poll = (id: string, at: number) => req({ requestId: id, url: 'https://api.example.com/poll', startedAt: at })
    expect(outsideActions([poll('1', 100), poll('2', 200), poll('3', 300)], [[190, 210]]).map((r) => r.requestId)).toEqual([
      '1',
      '3'
    ])
  })
})

describe('formatTriggeredRequests', () => {
  it('요청이 없어도 그 사실과 기준 시각을 한 줄로 알린다', () => {
    const out = formatTriggeredRequests({ shown: [], hidden: 0 }, 1000)
    expect(out).toContain('no XHR/Fetch requests triggered by this action')
    expect(out).toContain('since=1000')
  })

  it('요청을 메서드·상태·URL 한 줄로 포맷하고 개수와 기준 시각을 머리말에 넣는다', () => {
    const out = formatTriggeredRequests(
      { shown: [req({ requestId: '1', method: 'POST', status: 200, url: 'https://api.example.com/login' })], hidden: 0 },
      1000
    )
    expect(out).toContain('requests triggered by this action (1, since=1000)')
    expect(out).toContain('#1 POST 200 https://api.example.com/login')
  })

  it('상태 코드가 없으면 — 로 표시한다', () => {
    const out = formatTriggeredRequests({ shown: [req({ requestId: '2', status: undefined })], hidden: 0 }, 0)
    expect(out).toContain('#2 GET — ')
  })

  it('숨긴 배경 요청이 있으면 개수와 전체 보기 방법을 알린다', () => {
    const out = formatTriggeredRequests({ shown: [], hidden: 3 }, 1000)
    expect(out).toContain('3 recurring background request(s) hidden')
    expect(out).toContain('list_requests({"since":1000})')
  })

  it('최대 개수를 넘으면 나머지를 요약한다', () => {
    const shown = Array.from({ length: 23 }, (_, i) => req({ requestId: String(i) }))
    const out = formatTriggeredRequests({ shown, hidden: 0 }, 0, 20)
    expect(out).toContain('requests triggered by this action (23, since=0)')
    expect(out.split('\n').filter((l) => l.startsWith('#')).length).toBe(20)
    expect(out).toContain('… +3 more')
  })
})
