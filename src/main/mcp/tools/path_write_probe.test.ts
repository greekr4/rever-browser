import { describe, expect, it } from 'vitest'

import { buildPathWritePlan, pathWriteDecision, placeMarkers } from './path_write_probe'

describe('buildPathWritePlan', () => {
  it('평문/트래버설 파일명과 서로 다른 마커를 만든다', () => {
    const p = buildPathWritePlan('zABCz')
    expect(p.fname1).toBe('rpwf1zABCz')
    expect(p.fname2).toBe('../rpwf2zABCz')
    expect(p.marker1).not.toBe(p.marker2)
    // 트래버설만 ../ 를 가진다
    expect(p.fname1.includes('..')).toBe(false)
    expect(p.fname2.startsWith('../')).toBe(true)
  })
})

describe('pathWriteDecision', () => {
  it('평문 보이고 트래버설 안 보이면 traversal 확정', () => {
    expect(pathWriteDecision(true, false)).toEqual({ confirmed: true, kind: 'traversal' })
  })
  it('둘 다 보이면 저장은 되나 traversal 미확정', () => {
    expect(pathWriteDecision(true, true)).toEqual({ confirmed: false, kind: 'stored-no-traversal' })
  })
  it('평문도 안 보이면 inconclusive (되읽기 훅 없음)', () => {
    expect(pathWriteDecision(false, false)).toEqual({ confirmed: false, kind: 'inconclusive' })
    expect(pathWriteDecision(false, true)).toEqual({ confirmed: false, kind: 'inconclusive' })
  })
})

describe('placeMarkers', () => {
  it('테스트 파라미터는 파일명, 나머지 쿼리 파라미터는 마커로 채운다 (Testimonial GET)', () => {
    const r = placeMarkers(
      'http://localhost:1337/?customer=&testimonial=',
      undefined,
      'customer',
      '../evil',
      'MARK'
    )
    const u = new URL(r.url)
    expect(u.searchParams.get('customer')).toBe('../evil')
    expect(u.searchParams.get('testimonial')).toBe('MARK')
  })
  it('쿼리에 없던 테스트 파라미터는 추가한다', () => {
    const r = placeMarkers('http://x/?a=1', undefined, 'customer', 'fn', 'MARK')
    const u = new URL(r.url)
    expect(u.searchParams.get('a')).toBe('MARK')
    expect(u.searchParams.get('customer')).toBe('fn')
  })
  it('폼 바디의 다른 필드도 마커로 채운다', () => {
    const r = placeMarkers('http://x/', 'name=&content=', 'name', '../fn', 'MARK')
    const sp = new URLSearchParams(r.body)
    expect(sp.get('name')).toBe('../fn')
    expect(sp.get('content')).toBe('MARK')
  })
})
