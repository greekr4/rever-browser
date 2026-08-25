import { describe, it, expect, beforeEach } from 'vitest'

import {
  encodeRefetchedBody,
  getWasmBuffer,
  listWasm,
  decompileRequest
} from './wasm-analysis'
import { upsertRequest, clearTraffic } from '../traffic-store'

const WASM_MAGIC = Buffer.from([0x00, 0x61, 0x73, 0x6d]) // "\0asm"

// U1 — binary-body encoding decision (the refetchBody fix)
describe('encodeRefetchedBody', () => {
  it('base64-encodes a wasm body and preserves the magic bytes', () => {
    const buf = Buffer.concat([WASM_MAGIC, Buffer.from([0x01, 0x00, 0x00, 0x00])]) // + version
    const out = encodeRefetchedBody(buf, 'application/wasm')
    expect(out.responseBodyBase64).toBe(true)
    const round = Buffer.from(out.responseBody, 'base64')
    expect(round.subarray(0, 4)).toEqual(WASM_MAGIC) // 00 61 73 6d
  })

  it('keeps text bodies as utf8', () => {
    const out = encodeRefetchedBody(Buffer.from('{"ok":true}', 'utf8'), 'application/json')
    expect(out.responseBodyBase64).toBe(false)
    expect(out.responseBody).toBe('{"ok":true}')
  })
})

// U2 — captured-body → Buffer accessor
describe('getWasmBuffer', () => {
  beforeEach(() => {
    clearTraffic()
    upsertRequest({
      requestId: 'w1',
      url: 'http://127.0.0.1:8779/sign.wasm',
      host: '127.0.0.1',
      resourceType: 'Other',
      mimeType: 'application/wasm',
      responseBody: Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]).toString('base64'),
      responseBodyBase64: true
    })
  })

  it('decodes a base64 wasm body to a Buffer with the magic header', () => {
    const buf = getWasmBuffer('w1')
    expect(Buffer.isBuffer(buf)).toBe(true)
    expect((buf as Buffer).subarray(0, 4)).toEqual(WASM_MAGIC)
  })

  it('returns an error string for an unknown requestId', () => {
    expect(typeof getWasmBuffer('nope')).toBe('string')
  })
})

// U3 — list_wasm filtering
describe('listWasm', () => {
  beforeEach(() => {
    clearTraffic()
    upsertRequest({ requestId: 'a', url: 'http://h/sign.wasm', host: 'h', resourceType: 'Other', mimeType: 'application/wasm', responseBody: '...', responseBodyBase64: true })
    upsertRequest({ requestId: 'b', url: 'http://h/app.js', host: 'h', resourceType: 'Script', mimeType: 'application/javascript', responseBody: 'x'.repeat(50) })
    upsertRequest({ requestId: 'c', url: 'http://h/logo.png', host: 'h', resourceType: 'Image', mimeType: 'image/png', responseBody: '...', responseBodyBase64: true })
    upsertRequest({ requestId: 'd', url: 'http://h/m2.wasm', host: 'h', resourceType: 'Other', mimeType: 'application/wasm', responseBody: '...', responseBodyBase64: true, responseBodyTruncated: true })
  })

  it('returns only wasm entries (mimeType or .wasm URL), excluding JS/images', () => {
    const ids = listWasm().map((r) => r.requestId).sort()
    expect(ids).toEqual(['a', 'd'])
  })

  it('surfaces the truncated flag', () => {
    expect(listWasm().find((r) => r.requestId === 'd')?.responseBodyTruncated).toBe(true)
  })
})

// U4 — tool error paths
describe('decompileRequest error paths', () => {
  beforeEach(() => {
    clearTraffic()
    upsertRequest({
      requestId: 'w1',
      url: 'http://127.0.0.1:8779/sign.wasm',
      host: '127.0.0.1',
      resourceType: 'Other',
      mimeType: 'application/wasm',
      responseBody: Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]).toString('base64'),
      responseBodyBase64: true
    })
  })

  it('returns an err for an unknown requestId (does not throw)', async () => {
    const res = await decompileRequest('nope', false)
    expect('isError' in res && res.isError).toBe(true)
    expect(res.content[0].text).toMatch(/unknown requestId/)
  })

  it('returns an install hint when wabt cannot be loaded (does not throw)', async () => {
    const res = await decompileRequest('w1', false, () => Promise.reject(new Error('module missing')))
    expect('isError' in res && res.isError).toBe(true)
    expect(res.content[0].text).toMatch(/wabt.*install/i)
  })
})
