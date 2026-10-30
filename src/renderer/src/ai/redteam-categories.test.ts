import { describe, expect, it } from 'vitest'

import { checklistFor, classify } from './redteam-categories'

const ids = (method: string, url: string) => classify(method, url).map((c) => c.id)

describe('redteam-categories classify', () => {
  it('파라미터 없는 GET 은 인증 재생만 적용된다', () => {
    expect(ids('GET', 'https://ex.com/health')).toEqual(['broken_auth'])
  })

  it('경로에 숫자 id 가 있으면 IDOR 를 적용한다', () => {
    expect(ids('GET', 'https://ex.com/users/42')).toContain('idor')
  })

  it('uuid 경로도 IDOR 로 본다', () => {
    expect(ids('GET', 'https://ex.com/orders/550e8400-e29b-41d4-a716-446655440000')).toContain('idor')
  })

  it('GET 쿼리 파라미터가 있으면 XSS·CRLF 를 적용하고, id 류가 아니면 IDOR 는 아니다', () => {
    const c = ids('GET', 'https://ex.com/search?q=hi')
    expect(c).toContain('xss')
    expect(c).toContain('crlf')
    expect(c).not.toContain('idor')
  })

  it('POST 는 XSS 를 적용하지 않지만 CRLF 는 적용한다', () => {
    const c = ids('POST', 'https://ex.com/submit?token=1')
    expect(c).not.toContain('xss')
    expect(c).toContain('crlf')
  })

  it('url 류 파라미터가 있으면 SSRF 를 적용한다', () => {
    expect(ids('GET', 'https://ex.com/fetch?redirect=https://x')).toContain('ssrf')
    expect(ids('GET', 'https://ex.com/img?image=a.png')).toContain('ssrf')
  })

  it('파일/경로 파라미터가 있으면 LFI 를 적용한다', () => {
    expect(ids('GET', 'https://ex.com/view?file=report.pdf')).toContain('lfi')
  })

  it('파싱 불가 URL 은 빈 배열', () => {
    expect(classify('GET', 'not a url')).toEqual([])
  })
})

describe('checklistFor', () => {
  it('적용 카테고리를 OWASP 라벨·태그와 함께 나열한다', () => {
    const s = checklistFor('GET', 'https://ex.com/users/42?redirect=x')
    expect(s).toContain('API1:BOLA/IDOR')
    expect(s).toContain('API7:SSRF')
    expect(s).toContain('tags:["idor"]')
  })

  it('해당 없으면 빈 문자열', () => {
    expect(checklistFor('GET', 'not a url')).toBe('')
  })
})
