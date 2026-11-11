import { describe, expect, it } from 'vitest'

import { withCookie, originalCookie, sameResource, idorDecision, noauthDecision, bogusCookie, type IdorReply } from './idor_probe'

const reply = (label: IdorReply['label'], status: number, body: string): IdorReply => ({ label, status, body })

describe('withCookie', () => {
  it('기존 Cookie 와 buildRawRequest 가 관리하는 헤더를 버리고 새 쿠키를 넣는다', () => {
    const h = { Accept: 'application/json', Cookie: 'old=1', Host: 'x', 'Content-Length': '5', Connection: 'keep-alive' }
    expect(withCookie(h, 'ats_token=B')).toEqual({ Accept: 'application/json', Cookie: 'ats_token=B' })
  })

  it('cookie=null 이면 Cookie 를 아예 넣지 않는다 (무인증)', () => {
    expect(withCookie({ Accept: 'json', cookie: 'old=1' }, null)).toEqual({ Accept: 'json' })
  })

  it('대소문자 무관하게 기존 쿠키를 교체한다', () => {
    expect(withCookie({ COOKIE: 'old=1' }, 'new=2')).toEqual({ Cookie: 'new=2' })
  })
})

describe('originalCookie', () => {
  it('캡처 요청의 Cookie 값을 대소문자 무관하게 뽑는다', () => {
    expect(originalCookie({ cookie: 'ats_token=A' })).toBe('ats_token=A')
    expect(originalCookie({ Accept: 'json' })).toBeNull()
  })
})

describe('sameResource', () => {
  it('공백 차이는 무시하고 같은 리소스로 본다', () => {
    expect(sameResource('{"id":"abc","name":"X"}', '{ "id": "abc", "name": "X" }')).toBe(true)
  })

  it('한쪽이 상대의 앞 64자 지문을 포함하면 같은 리소스', () => {
    const owner = '{"id":"cdb11ac2-9993-4ac6-bb9d-6959df5228a9","applicants":[{"email":"a@b.com"}]}'
    expect(sameResource(owner, 'prefix...' + owner + '...suffix')).toBe(true)
  })

  it('다른 리소스면 false', () => {
    expect(sameResource('{"id":"abc","data":1}', '{"error":"forbidden"}')).toBe(false)
  })

  it('너무 짧은 본문은 판정하지 않는다 — 오탐 방지', () => {
    expect(sameResource('{}', '{}')).toBe(false)
  })
})

describe('idorDecision', () => {
  const ownerBody = '{"id":"cdb11ac2-9993-4ac6-bb9d-6959df5228a9","applicants":[{"email":"uoojin12@naver.com"}]}'

  it('익명 거부 + 타테넌트가 같은 리소스 읽으면 BOLA 확정', () => {
    const out = idorDecision(
      reply('baseline', 200, ownerBody),
      reply('noauth', 401, '{"message":"Unauthorized"}'),
      reply('cross', 200, ownerBody)
    )
    expect(out.bola).toBe(true)
  })

  it('익명도 같은 리소스를 주면 BOLA 아니라 공개 노출', () => {
    const out = idorDecision(
      reply('baseline', 200, ownerBody),
      reply('noauth', 200, ownerBody),
      reply('cross', 200, ownerBody)
    )
    expect(out.bola).toBe(false)
    expect(out.note).toMatch(/public-exposure|broken-auth|info-disclosure/i)
  })

  it('타테넌트가 403 이면 소유자 범위로 판정 — 음성', () => {
    const out = idorDecision(
      reply('baseline', 200, ownerBody),
      reply('noauth', 401, 'no'),
      reply('cross', 403, '{"message":"Forbidden"}')
    )
    expect(out.bola).toBe(false)
  })

  it('타테넌트가 200 이지만 자기 데이터(다른 본문)면 음성', () => {
    const out = idorDecision(
      reply('baseline', 200, ownerBody),
      reply('noauth', 401, 'no'),
      reply('cross', 200, '{"id":"other","applicants":[]}')
    )
    expect(out.bola).toBe(false)
  })

  it('소유자 baseline 이 2xx 가 아니면 판정 불가', () => {
    const out = idorDecision(
      reply('baseline', 404, 'not found'),
      reply('noauth', 401, 'no'),
      reply('cross', 200, ownerBody)
    )
    expect(out.bola).toBe(false)
    expect(out.note).toMatch(/baseline/i)
  })
})

describe('noauthDecision', () => {
  const ownerBody = '{"id":"cdb11ac2-9993-4ac6-bb9d-6959df5228a9","member":{"email":"te@test.com"}}'

  it('무쿠키로 보호 리소스가 오고 쓰레기 쿠키는 거부되면 인증 우회 확정', () => {
    const out = noauthDecision(reply('baseline', 200, ownerBody), reply('noauth', 200, ownerBody), reply('bogus', 401, '{"message":"Unauthorized"}'))
    expect(out.brokenAuth).toBe(true)
  })

  it('무쿠키·쓰레기쿠키 모두 같은 리소스를 주면 공개-설계 → 확정 안 함 (오탐 방지)', () => {
    const out = noauthDecision(reply('baseline', 200, ownerBody), reply('noauth', 200, ownerBody), reply('bogus', 200, ownerBody))
    expect(out.brokenAuth).toBe(false)
    expect(out.note).toMatch(/public-by-design/i)
  })

  it('쿠키 없으면 401 이면 정상 보호 — 음성 (httpOnly 재첨부 오탐 방지)', () => {
    const out = noauthDecision(reply('baseline', 200, ownerBody), reply('noauth', 401, '{"message":"Unauthorized"}'), reply('bogus', 401, 'no'))
    expect(out.brokenAuth).toBe(false)
  })

  it('무쿠키 200 이지만 빈/다른 본문(session:null 류)이면 음성', () => {
    const out = noauthDecision(reply('baseline', 200, ownerBody), reply('noauth', 200, '{"session":null}'), reply('bogus', 200, '{"session":null}'))
    expect(out.brokenAuth).toBe(false)
  })

  it('baseline 이 2xx 아니면 판정 불가', () => {
    const out = noauthDecision(reply('baseline', 500, 'err'), reply('noauth', 401, 'no'), reply('bogus', 401, 'no'))
    expect(out.brokenAuth).toBe(false)
    expect(out.note).toMatch(/baseline/i)
  })
})

describe('bogusCookie', () => {
  it('쿠키 이름은 유지하고 값만 쓰레기로 바꾼다', () => {
    expect(bogusCookie('session=abc123; ats_token=eyJhbGc.iJ')).toBe('session=reverbogus0; ats_token=reverbogus0')
  })
})
