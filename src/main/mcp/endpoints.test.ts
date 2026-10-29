import { describe, it, expect } from 'vitest'

import { summarizeEndpoints, formatEndpoints, splitBySite } from './endpoints'
import type { StoredRequest } from '../traffic-store'

function req(partial: Partial<StoredRequest> & { requestId: string }): StoredRequest {
  return {
    url: 'https://api.example.com/x',
    host: 'api.example.com',
    method: 'GET',
    resourceType: 'Fetch',
    startedAt: 0,
    ...partial
  }
}

describe('summarizeEndpoints', () => {
  it('숫자 ID 경로를 :id로 묶어 한 엔드포인트로 센다', () => {
    const eps = summarizeEndpoints([
      req({ requestId: '1', url: 'https://api.example.com/users/12/orders' }),
      req({ requestId: '2', url: 'https://api.example.com/users/34/orders' })
    ])
    expect(eps).toHaveLength(1)
    expect(eps[0]).toMatchObject({ method: 'GET', path: '/users/:id/orders', count: 2 })
  })

  it('XHR/Fetch만 집계한다', () => {
    const eps = summarizeEndpoints([
      req({ requestId: '1', resourceType: 'Image', url: 'https://cdn.example.com/a.png' }),
      req({ requestId: '2', resourceType: 'Script', url: 'https://cdn.example.com/app.js' }),
      req({ requestId: '3', resourceType: 'XHR', url: 'https://api.example.com/feed' })
    ])
    expect(eps.map((e) => e.path)).toEqual(['/feed'])
  })

  it('메서드가 다르면 다른 엔드포인트다', () => {
    const eps = summarizeEndpoints([
      req({ requestId: '1', method: 'GET', url: 'https://api.example.com/cart' }),
      req({ requestId: '2', method: 'POST', url: 'https://api.example.com/cart' })
    ])
    expect(eps).toHaveLength(2)
  })

  it('호출 수가 많은 순으로 정렬하고 상태 코드와 마지막 요청 ID를 모은다', () => {
    const eps = summarizeEndpoints([
      req({ requestId: '1', url: 'https://api.example.com/once', status: 200, startedAt: 1 }),
      req({ requestId: '2', url: 'https://api.example.com/many', status: 200, startedAt: 2 }),
      req({ requestId: '3', url: 'https://api.example.com/many', status: 500, startedAt: 3 })
    ])
    expect(eps[0]).toMatchObject({ path: '/many', count: 2, statuses: [200, 500], lastRequestId: '3' })
    expect(eps[1].path).toBe('/once')
  })
})

describe('formatEndpoints', () => {
  it('엔드포인트가 없으면 안내 문구를 반환한다', () => {
    expect(formatEndpoints([], 50)).toContain('No XHR/Fetch')
  })

  it('한 줄에 메서드·URL·호출 수·상태·마지막 요청 ID를 쓴다', () => {
    const out = formatEndpoints(
      summarizeEndpoints([req({ requestId: '9', method: 'POST', url: 'https://api.example.com/cart', status: 200 })]),
      50
    )
    expect(out).toContain('POST https://api.example.com/cart ×1 [200] last=#9')
  })

  it('limit을 넘으면 나머지 개수를 요약한다', () => {
    const reqs = Array.from({ length: 5 }, (_, i) => req({ requestId: String(i), url: `https://api.example.com/e${i}` }))
    const out = formatEndpoints(summarizeEndpoints(reqs), 3)
    expect(out.split('\n').filter((l) => l.startsWith('GET ')).length).toBe(3)
    expect(out).toContain('… +2 more')
  })
})

describe('splitBySite', () => {
  it('현재 페이지와 같은 사이트(eTLD+1)의 요청만 남기고 나머지를 따로 돌려준다', () => {
    const { same, other } = splitBySite(
      [
        req({ requestId: '1', url: 'https://api.example.com/feed' }),
        req({ requestId: '2', url: 'https://rake.11st.co.kr/log' }),
        req({ requestId: '3', url: 'https://www.google-analytics.com/g/collect' })
      ],
      'https://www.example.com/home'
    )
    expect(same.map((r) => r.requestId)).toEqual(['1'])
    expect(other.map((r) => r.requestId)).toEqual(['2', '3'])
  })

  it('localhost는 포트가 달라도 같은 사이트다', () => {
    const { same } = splitBySite(
      [req({ requestId: '1', url: 'http://localhost:3000/v1/jobs' })],
      'http://localhost:8080/my'
    )
    expect(same).toHaveLength(1)
  })

  it('페이지 URL이 http(s)가 아니면 거르지 않는다', () => {
    const { same, other } = splitBySite([req({ requestId: '1' })], 'about:blank')
    expect(same).toHaveLength(1)
    expect(other).toHaveLength(0)
  })
})

describe('formatEndpoints 다른 사이트 안내', () => {
  it('숨긴 다른 사이트 엔드포인트 수와 전체 보기 방법을 알린다', () => {
    const out = formatEndpoints(summarizeEndpoints([req({ requestId: '1' })]), 50, { site: 'example.com', otherSites: 7 })
    expect(out).toContain('site example.com')
    expect(out).toContain('7 endpoint(s) on other sites hidden — pass all=true')
  })
})
