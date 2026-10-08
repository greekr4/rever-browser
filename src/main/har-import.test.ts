import fs from 'node:fs'

import { describe, expect, it } from 'vitest'

import { harToEntries } from './har-import'
import { toHeaders } from './mcp/har-build'

const chromeEntry = {
  startedDateTime: '2026-10-08T01:02:03.000Z',
  time: 120,
  _resourceType: 'fetch',
  _initiator: {
    type: 'script',
    stack: {
      callFrames: [
        { functionName: 'callApi', url: 'https://x.io/app.js', lineNumber: 4, columnNumber: 10 },
        { functionName: '', url: 'https://x.io/app.js', lineNumber: 9, columnNumber: 2 }
      ]
    }
  },
  request: {
    method: 'POST',
    url: 'https://api.x.io/v3/login?x=1',
    headers: [
      { name: 'content-type', value: 'application/json' },
      { name: 'authorization', value: 'Bearer t' }
    ],
    postData: { mimeType: 'application/json', text: '{"u":"me"}' }
  },
  response: {
    status: 200,
    headers: [
      { name: 'content-type', value: 'application/json; charset=utf-8' },
      { name: 'set-cookie', value: 'a=1' },
      { name: 'set-cookie', value: 'b=2' }
    ],
    content: { size: 4, mimeType: 'application/json; charset=utf-8', text: 'e30=', encoding: 'base64' },
    _transferSize: 321
  }
}

const har = (entries: unknown[]) => ({ log: { version: '1.2', creator: { name: 'WebInspector', version: '537' }, entries } })

describe('harToEntries', () => {
  it('Chrome HAR 항목을 StoredRequest로 바꾼다', () => {
    const { entries, skipped, creator } = harToEntries(har([chromeEntry]), 'imp1')
    expect(skipped).toEqual([])
    expect(creator).toBe('WebInspector 537')
    expect(entries[0]).toEqual({
      requestId: 'har:imp1:0',
      url: 'https://api.x.io/v3/login?x=1',
      host: 'api.x.io',
      method: 'POST',
      resourceType: 'Fetch',
      startedAt: Date.parse('2026-10-08T01:02:03.000Z'),
      completedAt: Date.parse('2026-10-08T01:02:03.000Z') + 120,
      status: 200,
      mimeType: 'application/json',
      encodedDataLength: 321,
      requestHeaders: { 'content-type': 'application/json', authorization: 'Bearer t' },
      requestPostData: '{"u":"me"}',
      responseHeaders: { 'content-type': 'application/json; charset=utf-8', 'set-cookie': 'a=1\nb=2' },
      responseBody: 'e30=',
      responseBodyBase64: true,
      initiatorType: 'script',
      initiatorStack: [
        { functionName: 'callApi', url: 'https://x.io/app.js', lineNumber: 4, columnNumber: 10 },
        { functionName: '', url: 'https://x.io/app.js', lineNumber: 9, columnNumber: 2 }
      ]
    })
  })

  it('_resourceType이 없으면 mimeType으로 종류를 추정한다', () => {
    const base = { startedDateTime: '2026-10-08T00:00:00Z', time: -1, request: { method: 'GET', url: 'https://x.io/a', headers: [] } }
    const kinds = ['application/javascript', 'text/html', 'application/json', 'text/css', 'image/png', 'font/woff2', 'application/zip'].map(
      (mimeType) =>
        harToEntries(har([{ ...base, response: { status: 200, headers: [], content: { mimeType } } }]), 'i').entries[0].resourceType
    )
    expect(kinds).toEqual(['Script', 'Document', 'Fetch', 'Stylesheet', 'Image', 'Font', 'Other'])
  })

  it('URL이 잘못된 항목은 건너뛰고 이유와 순번을 보고한다', () => {
    const bad = { ...chromeEntry, request: { ...chromeEntry.request, url: 'not a url' } }
    const { entries, skipped } = harToEntries(har([bad, chromeEntry]), 'i')
    expect(entries.map((e) => e.requestId)).toEqual(['har:i:1'])
    expect(skipped).toEqual([{ index: 0, reason: 'invalid request.url' }])
  })

  it('time이 음수면 completedAt을 두지 않는다', () => {
    const e = harToEntries(har([{ ...chromeEntry, time: -1 }]), 'i').entries[0]
    expect(e.completedAt).toBeUndefined()
  })

  it('HAR가 아니면 에러를 던진다', () => {
    expect(() => harToEntries({ foo: 1 }, 'i')).toThrow(/not a HAR/)
    expect(() => harToEntries(null, 'i')).toThrow(/not a HAR/)
  })

  it('har_export의 헤더 형식과 왕복해도 같다', () => {
    const record = { 'set-cookie': 'a=1\nb=2', accept: '*/*' }
    const entry = { ...chromeEntry, response: { ...chromeEntry.response, headers: toHeaders(record) } }
    expect(harToEntries(har([entry]), 'i').entries[0].responseHeaders).toEqual(record)
  })

  it('E2E용 실제 픽스처(shop-api.har)를 그대로 가져온다', () => {
    const raw = JSON.parse(fs.readFileSync('test-fixtures/har/shop-api.har', 'utf8'))
    const { entries, skipped } = harToEntries(raw, 'shop')
    expect(skipped).toEqual([])
    expect(entries.map((e) => [e.method, e.resourceType, e.status])).toEqual([
      ['GET', 'Document', 200],
      ['GET', 'Script', 200],
      ['POST', 'Fetch', 200],
      ['GET', 'Fetch', 200],
      ['GET', 'Fetch', 401]
    ])
    expect(entries[2].responseHeaders?.['set-cookie']).toBe('refresh=rt_abc; HttpOnly\nsid=s_123; Secure')
    expect(entries[3].requestHeaders?.['x-signature']).toBe('a1b2c3d4e5f60718293a4b5c6d7e8f90')
  })
})
