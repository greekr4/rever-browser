import { describe, it, expect } from 'vitest'

import { mask, scanText, scanRequest } from './secret-scan'
import { decodeToken, decodeBase64Url, formatExpiry } from './token-decode'
import { parseQuery, diffObjects, diffRequests, computeApiBase } from './request-diff'
import { percentile } from './stats'
import { toHeaders } from './har-build'
import type { StoredRequest } from '../traffic-store'

describe('secret-scan', () => {
  it('masks long values, keeps first/last 4 and the length', () => {
    expect(mask('short')).toBe('****')
    expect(mask('AKIA1234567890ABCDEF')).toBe('AKIA…CDEF (20 chars)')
  })

  it('finds an AWS key and a JWT, tagging where', () => {
    const jwt = 'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghij'
    const hits = scanText(`token=AKIAIOSFODNN7EXAMPLE and ${jwt}`, 'response-body')
    const types = hits.map((h) => h.type)
    expect(types).toContain('aws-access-key-id')
    expect(types).toContain('jwt')
    expect(hits.every((h) => h.where === 'response-body')).toBe(true)
  })

  it('reports sensitive request headers by name and dedupes', () => {
    const req = {
      requestId: 'r1',
      url: 'https://a.com',
      host: 'a.com',
      requestHeaders: { Authorization: 'Bearer abcdefghijklmnop' }
    } as unknown as StoredRequest
    const hits = scanRequest(req)
    expect(hits.some((h) => h.type === 'header:authorization')).toBe(true)
  })
})

describe('token-decode', () => {
  it('decodes a JWT and flags expiry', () => {
    // {"alg":"HS256"} . {"sub":"1","exp":1} . sig  → exp far in the past
    const header = decodeBase64Url('eyJhbGciOiJIUzI1NiJ9')
    expect(JSON.parse(header).alg).toBe('HS256')
    const jwt = 'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIiwiZXhwIjoxfQ.sig'
    const out = decodeToken(jwt)
    expect(out.type).toBe('jwt')
    expect(String(out.note)).toContain('EXPIRED')
  })

  it('decodes base64 json and url-encoded json', () => {
    expect(decodeToken('eyJhIjoxfQ==')).toMatchObject({ type: 'base64-json' })
    expect(decodeToken('%7B%22a%22%3A1%7D')).toMatchObject({ type: 'url-encoded-json' })
  })

  it('falls back to unknown', () => {
    expect(decodeToken('???').type).toBe('unknown')
  })

  it('formatExpiry ignores payloads without a numeric exp', () => {
    expect(formatExpiry({ sub: '1' })).toBe('')
  })
})

describe('request-diff', () => {
  it('parses query strings and returns {} for junk', () => {
    expect(parseQuery('https://a.com/x?a=1&b=2')).toEqual({ a: '1', b: '2' })
    expect(parseQuery('not a url')).toEqual({})
  })

  it('classifies added/removed/changed keys', () => {
    const changes = diffObjects('q', { a: 1, b: 2 }, { b: 3, c: 4 })
    expect(changes).toContainEqual({ type: 'removed', key: 'q.a', from: 1 })
    expect(changes).toContainEqual({ type: 'changed', key: 'q.b', from: 2, to: 3 })
    expect(changes).toContainEqual({ type: 'added', key: 'q.c', to: 4 })
  })

  it('diffs two requests across query, headers (lowercased) and json body', () => {
    const a = {
      url: 'https://a.com/x?p=1',
      requestHeaders: { 'X-Token': 'a' },
      requestPostData: '{"n":1}'
    } as unknown as StoredRequest
    const b = {
      url: 'https://a.com/x?p=2',
      requestHeaders: { 'x-token': 'b' },
      requestPostData: '{"n":2}'
    } as unknown as StoredRequest
    const changes = diffRequests(a, b)
    expect(changes).toContainEqual({ type: 'changed', key: 'query.p', from: '1', to: '2' })
    expect(changes).toContainEqual({ type: 'changed', key: 'headers.x-token', from: 'a', to: 'b' })
    expect(changes).toContainEqual({ type: 'changed', key: 'body.n', from: 1, to: 2 })
  })

  it('ranks the most common host + path prefix', () => {
    const bases = computeApiBase([
      'https://api.x.com/v1/users/1',
      'https://api.x.com/v1/users/2',
      'https://api.x.com/v2/orders'
    ])
    expect(bases[0]).toEqual({ base: 'https://api.x.com/v1/users', count: 2 })
  })
})

describe('stats.percentile', () => {
  it('uses nearest-rank on a sorted array', () => {
    const s = [10, 20, 30, 40, 50]
    expect(percentile(s, 0.5)).toBe(30)
    expect(percentile(s, 0.95)).toBe(40) // floor((5-1)*0.95)=3 → 40
    expect(percentile(s, 1)).toBe(50)
    expect(percentile([], 0.5)).toBe(0)
  })
})

describe('har-build.toHeaders', () => {
  it('maps a record to name/value pairs and handles undefined', () => {
    expect(toHeaders({ A: '1' })).toEqual([{ name: 'A', value: '1' }])
    expect(toHeaders(undefined)).toEqual([])
  })
})
