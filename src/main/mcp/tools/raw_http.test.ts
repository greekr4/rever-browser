import { describe, expect, it } from 'vitest'

import { buildRawRequest, parseRawResponse } from './raw_http'

describe('buildRawRequest', () => {
  it('request line 을 그대로 쓴다 — # fragment 미제거 (CVE-2023-45539)', () => {
    const req = buildRawRequest('GET', '/x#/healthcheck-dev', {}, undefined, '127.0.0.1:1337')
    expect(req.split('\r\n')[0]).toBe('GET /x#/healthcheck-dev HTTP/1.1')
    expect(req).toContain('Host: 127.0.0.1:1337')
    expect(req).toContain('Connection: close')
    expect(req.endsWith('\r\n\r\n')).toBe(true)
  })

  it('body 가 있으면 Content-Length 를 자동 계산', () => {
    const req = buildRawRequest('POST', '/x', { 'Content-Type': 'application/json' }, '{"a":1}', 'h:80')
    expect(req).toContain('Content-Length: 7')
    expect(req).toContain('Content-Type: application/json')
    expect(req.endsWith('\r\n\r\n{"a":1}')).toBe(true)
  })

  it('사용자가 준 Host/Connection/Content-Length 는 중복 추가하지 않는다', () => {
    const req = buildRawRequest('GET', '/', { Host: 'evil', Connection: 'keep-alive' }, undefined, 'real:80')
    expect(req).toContain('Host: evil')
    expect(req).not.toContain('Host: real:80')
    expect((req.match(/Connection:/g) ?? []).length).toBe(1)
  })
})

describe('parseRawResponse', () => {
  it('status line/헤더/바디를 분리한다', () => {
    const raw = Buffer.from(
      'HTTP/1.1 403 Forbidden\r\nContent-Type: text/html\r\nContent-Length: 2\r\n\r\nno',
      'latin1'
    )
    const r = parseRawResponse(raw)
    expect(r.statusLine).toBe('HTTP/1.1 403 Forbidden')
    expect(r.status).toBe(403)
    expect(r.headers['content-type']).toBe('text/html')
    expect(r.body).toBe('no')
  })

  it('바디 없는 응답', () => {
    const r = parseRawResponse(Buffer.from('HTTP/1.1 200 OK\r\nServer: x\r\n\r\n', 'latin1'))
    expect(r.status).toBe(200)
    expect(r.headers.server).toBe('x')
    expect(r.body).toBe('')
  })
})
