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

  it('UUID 경로 리소스는 idor_probe 체크라인(BOLA/IDOR)을 받는다', () => {
    const s = checklistFor('GET', 'https://ex.com/v1/ats/jobs/cdb11ac2-9993-4ac6-bb9d-6959df5228a9')
    expect(s).toContain('idor_probe')
    expect(s).toContain('API1:BOLA/IDOR')
  })

  it('uuid 경로도 IDOR 로 본다', () => {
    expect(ids('GET', 'https://ex.com/orders/550e8400-e29b-41d4-a716-446655440000')).toContain('idor')
  })

  it('GET 쿼리 파라미터가 있으면 XSS·SSTI·CMDI·SQLI·CRLF 를 적용하고, id 류가 아니면 IDOR 는 아니다', () => {
    const c = ids('GET', 'https://ex.com/search?q=hi')
    expect(c).toContain('xss')
    expect(c).toContain('ssti')
    expect(c).toContain('cmdi')
    expect(c).toContain('sqli')
    expect(c).toContain('crlf')
    expect(c).not.toContain('idor')
  })

  it('date/format 류 쿼리 파라미터는 CMDI 체크라인(cmd_probe, critical)을 받는다 — TimeKORP 회귀', () => {
    const s = checklistFor('GET', 'http://localhost:1337/?format=%Y')
    expect(s).toContain('cmd_probe')
    expect(s).toContain('WSTG-INPV:OS Command Injection')
    expect(s).toContain('severity:"critical"')
  })

  it('로그인 폼 POST 는 SQLI 체크라인(sqli_probe, critical)을 받는다 — KORP Terminal 회귀', () => {
    const s = checklistFor('POST', 'http://localhost:1337/')
    expect(s).toContain('sqli_probe')
    expect(s).toContain('WSTG-INPV:SQL Injection')
    expect(s).toContain('severity:"critical"')
  })

  it('로그인 폼 POST 는 NoSQL 체크라인(nosqli_probe, critical)도 받는다 — Mongo/Neo4j 스택', () => {
    const s = checklistFor('POST', 'http://localhost:1337/')
    expect(s).toContain('nosqli_probe')
    expect(s).toContain('WSTG-INPV:NoSQL / Cypher Injection')
    expect(s).toContain('severity:"critical"')
  })

  it('반사 가능 엔드포인트는 sqli 와 nosqli 를 함께 적용한다', () => {
    const c = ids('GET', 'https://ex.com/search?q=hi')
    expect(c).toContain('sqli')
    expect(c).toContain('nosqli')
  })

  it('다중 필드 저장 폼은 path_write_probe 라인을 받는다 — Testimonial 회귀', () => {
    const s = checklistFor('GET', 'http://localhost:1337/?customer=a&testimonial=b')
    expect(s).toContain('path_write_probe')
    expect(s).toContain('WSTG-ATHZ:Path Traversal / Arbitrary File Write')
  })

  it('단일 쿼리 파라미터(검색)는 path_write_probe 를 받지 않는다 — 과발사 방지', () => {
    expect(ids('GET', 'https://ex.com/search?q=hi')).not.toContain('pathwrite')
  })

  it('POST 는 바디가 반사 싱크이므로 XSS·SSTI 를 적용한다(쿼리 없어도)', () => {
    const c = ids('POST', 'https://ex.com/')
    expect(c).toContain('xss')
    expect(c).toContain('ssti')
  })

  it('파라미터 없는 GET 은 반사 체크(XSS/SSTI)를 적용하지 않는다', () => {
    const c = ids('GET', 'https://ex.com/health')
    expect(c).not.toContain('xss')
    expect(c).not.toContain('ssti')
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
