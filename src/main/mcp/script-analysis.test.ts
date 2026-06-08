import { describe, it, expect, beforeEach } from 'vitest'

import { grepBody, detectBundler, patternForCategory, listScripts } from './script-analysis'
import { upsertRequest, clearTraffic } from '../traffic-store'

describe('grepBody', () => {
  const opts = { max: 100, before: 0, after: 0 }

  it('finds matches with surrounding snippet context', () => {
    const hits = grepBody('abcTOKENdef', /TOKEN/g, { max: 10, before: 2, after: 2 })
    expect(hits).toHaveLength(1)
    expect(hits[0].match).toBe('TOKEN')
    expect(hits[0].offset).toBe(3)
    expect(hits[0].snippet).toBe('bcTOKENde')
  })

  it('dedupes identical matches inside the same 200-byte bucket', () => {
    const body = 'TOKEN' + ' '.repeat(10) + 'TOKEN'
    expect(grepBody(body, /TOKEN/g, opts)).toHaveLength(1)
  })

  it('keeps identical matches in different 200-byte buckets', () => {
    const body = 'TOKEN' + ' '.repeat(250) + 'TOKEN'
    expect(grepBody(body, /TOKEN/g, opts)).toHaveLength(2)
  })

  it('respects the max cap', () => {
    const body = ['TOKEN', 'TOKEN', 'TOKEN'].join(' '.repeat(250))
    expect(grepBody(body, /TOKEN/g, { max: 2, before: 0, after: 0 })).toHaveLength(2)
  })

  it('adds the global flag when the source regex lacks it', () => {
    // patternForCategory regexes are global, but a caller may pass a non-global one.
    const body = 'TOKEN' + ' '.repeat(250) + 'TOKEN'
    expect(grepBody(body, /TOKEN/, opts)).toHaveLength(2)
  })

  it('filters out noise hosts for the urls category', () => {
    const body = '"https://w3.org/ns" and "https://api.example.com/v1/users"'
    const hits = grepBody(body, patternForCategory('urls'), opts)
    const matched = hits.map((h) => h.match)
    expect(matched.some((m) => m.includes('api.example.com'))).toBe(true)
    expect(matched.some((m) => m.includes('w3.org'))).toBe(false)
  })
})

describe('patternForCategory', () => {
  it('returns a global regex that matches its category', () => {
    const re = patternForCategory('api')
    expect(re.flags).toContain('g')
    expect('fetch("/api/users")').toMatch(patternForCategory('api'))
  })
})

describe('detectBundler', () => {
  it('detects vite via import.meta', () => {
    expect(detectBundler('const x = import.meta.url').name).toBe('vite')
  })

  it('detects webpack via __webpack_require__', () => {
    expect(detectBundler('function f(){ return __webpack_require__(0) }').name).toBe('webpack')
  })

  it('detects turbopack via its marker', () => {
    expect(detectBundler('/* turbopack runtime */').name).toBe('turbopack')
  })

  it('returns unknown for plain code', () => {
    expect(detectBundler('console.log(1 + 1)').name).toBe('unknown')
  })
})

describe('listScripts', () => {
  beforeEach(() => {
    clearTraffic()
    upsertRequest({ requestId: 's1', url: 'https://a/1.js', host: 'a', resourceType: 'Script', responseBody: 'a'.repeat(100) })
    upsertRequest({ requestId: 's2', url: 'https://a/2.js', host: 'a', resourceType: 'Script', responseBody: 'b'.repeat(300) })
    upsertRequest({ requestId: 's3', url: 'https://a/3.js', host: 'a', resourceType: 'Script', responseBody: 'c'.repeat(50) })
    // excluded: base64-encoded body
    upsertRequest({ requestId: 's4', url: 'https://a/4.js', host: 'a', resourceType: 'Script', responseBody: 'x'.repeat(400), responseBodyBase64: true })
    // excluded: not a script
    upsertRequest({ requestId: 's5', url: 'https://a/data', host: 'a', resourceType: 'XHR', responseBody: 'd'.repeat(500) })
    // excluded: no body
    upsertRequest({ requestId: 's6', url: 'https://a/6.js', host: 'a', resourceType: 'Script' })
  })

  it('returns script bodies sorted by size (largest first), excluding base64 / non-script / bodyless', () => {
    expect(listScripts().map((r) => r.requestId)).toEqual(['s2', 's1', 's3'])
  })

  it('applies the minSize filter', () => {
    expect(listScripts({ minSize: 100 }).map((r) => r.requestId)).toEqual(['s2', 's1'])
  })
})
