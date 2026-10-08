import { describe, expect, it, vi } from 'vitest'

vi.mock('./chrome-cdp', () => ({ getActiveTarget: () => null }))
vi.mock('./traffic-store', () => ({ getRequest: () => undefined }))

import { buildRequestSpec } from './repeater'

describe('repeater', () => {
  describe('buildRequestSpec (requestId 없이 URL만으로)', () => {
    it('modifications.url·method·headers·body로 요청을 만든다', () => {
      const spec = buildRequestSpec(undefined, {
        url: 'http://localhost:8779/api/v3/echo',
        method: 'post',
        setHeaders: { 'content-type': 'application/json', Origin: 'http://evil', 'X-Trace': '1' },
        body: '{"a":1}'
      })
      expect(spec).toEqual({
        url: 'http://localhost:8779/api/v3/echo',
        method: 'POST',
        headers: { 'content-type': 'application/json', 'X-Trace': '1' },
        body: '{"a":1}'
      })
    })

    it('method를 생략하면 GET, body null이면 body 없음', () => {
      const spec = buildRequestSpec(undefined, { url: 'http://x.io/', body: null })
      expect(spec.method).toBe('GET')
      expect(spec.body).toBeUndefined()
    })

    it('requestId도 url도 없으면 에러를 던진다', () => {
      expect(() => buildRequestSpec(undefined, {})).toThrow(/requestId or modifications\.url/)
      expect(() => buildRequestSpec(undefined, undefined)).toThrow(/requestId or modifications\.url/)
    })
  })
})
