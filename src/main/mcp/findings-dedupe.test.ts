import { describe, expect, it } from 'vitest'

import { endpointKeyOf, mergeDuplicateEndpoints, type DedupableFinding } from './findings-dedupe'

function f(over: Partial<DedupableFinding> & Pick<DedupableFinding, 'title' | 'body'>): DedupableFinding {
  return {
    id: Math.random().toString(36).slice(2),
    category: 'endpoint',
    severity: 'low',
    status: 'candidate',
    tags: [],
    evidenceIds: [],
    createdAt: Date.now(),
    ...over
  }
}

describe('endpointKeyOf', () => {
  it('같은 host+path 는 같은 키를 낸다 (쿼리/프래그먼트 제거)', () => {
    const a = endpointKeyOf(f({ title: '댓글 API', body: 'GET https://site.com/api/comments?slug=x' }))
    const b = endpointKeyOf(f({ title: 'site.com — comments API (/api/comments)', body: '다른 관측' }))
    expect(a).toBe('site.com/api/comments')
    expect(b).toBe('site.com/api/comments')
  })

  it('URL 이 없으면 첫 절대경로를 키로 쓴다', () => {
    const k = endpointKeyOf(f({ title: 'calc — 비인증 API (/api/comments)', body: '관측만' }))
    expect(k).toBe('/api/comments')
  })

  it('endpoint 카테고리가 아니면 키가 없다 (취약점끼리 병합 금지)', () => {
    const k = endpointKeyOf(f({ title: 'SQLi in /api/comments', body: 'https://x.com/api/comments', category: 'vuln' }))
    expect(k).toBeNull()
  })
})

describe('mergeDuplicateEndpoints', () => {
  it('같은 엔드포인트 3개를 1개로 병합하고 근거는 합친다', () => {
    const list = [
      f({ title: 'A', body: 'GET https://x.com/api/comments — 짧은 관측', evidenceIds: ['1'], tags: ['a'] }),
      f({ title: 'B', body: 'x.com — comments (/api/comments)\n훨씬 더 긴 관측 본문이라 이게 primary 가 된다', evidenceIds: ['2'], tags: ['b'], severity: 'medium' }),
      f({ title: 'C', body: 'https://x.com/api/comments?slug=y 중간', evidenceIds: ['3'] })
    ]
    const { merged, mergedAway } = mergeDuplicateEndpoints(list)
    expect(merged).toHaveLength(1)
    expect(mergedAway).toBe(2)
    expect(merged[0].body).toContain('primary')
    expect([...merged[0].evidenceIds].sort()).toEqual(['1', '2', '3'])
    expect([...merged[0].tags].sort()).toEqual(['a', 'b'])
    expect(merged[0].severity).toBe('medium')
  })

  it('다른 엔드포인트는 병합하지 않는다', () => {
    const list = [
      f({ title: 'A', body: 'https://x.com/api/users' }),
      f({ title: 'B', body: 'https://x.com/api/orders' })
    ]
    const { merged, mergedAway } = mergeDuplicateEndpoints(list)
    expect(merged).toHaveLength(2)
    expect(mergedAway).toBe(0)
  })

  it('vuln 카테고리는 같은 URL 이어도 병합하지 않는다', () => {
    const list = [
      f({ title: 'SQLi', body: 'https://x.com/api/q', category: 'vuln' }),
      f({ title: 'IDOR', body: 'https://x.com/api/q', category: 'vuln' })
    ]
    const { merged } = mergeDuplicateEndpoints(list)
    expect(merged).toHaveLength(2)
  })
})
