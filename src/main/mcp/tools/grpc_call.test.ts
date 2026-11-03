import { describe, expect, it } from 'vitest'

import {
  encodeVarint,
  encodeField,
  encodeMessage,
  frameMessage,
  unframeMessage,
  extractStringFields
} from './grpc_call'

describe('encodeVarint', () => {
  it('표준 LEB128 인코딩', () => {
    expect([...encodeVarint(0)]).toEqual([0x00])
    expect([...encodeVarint(1)]).toEqual([0x01])
    expect([...encodeVarint(127)]).toEqual([0x7f])
    expect([...encodeVarint(128)]).toEqual([0x80, 0x01])
    expect([...encodeVarint(300)]).toEqual([0xac, 0x02])
  })
})

describe('encodeField', () => {
  it('string 필드는 wire type 2 (key, len, bytes)', () => {
    // field 1, "ab" → key=(1<<3|2)=0x0a, len=2, 'a','b'
    expect([...encodeField({ number: 1, type: 'string', value: 'ab' })]).toEqual([0x0a, 0x02, 0x61, 0x62])
  })
  it('int 필드는 wire type 0 (key, varint)', () => {
    // field 2, 300 → key=(2<<3|0)=0x10, varint(300)=ac 02
    expect([...encodeField({ number: 2, type: 'int', value: 300 })]).toEqual([0x10, 0xac, 0x02])
  })
  it('bool 은 varint 0/1', () => {
    expect([...encodeField({ number: 3, type: 'bool', value: true })]).toEqual([0x18, 0x01])
    expect([...encodeField({ number: 3, type: 'bool', value: false })]).toEqual([0x18, 0x00])
  })
  it('bytes 는 base64 를 디코드해 넣는다', () => {
    // field 1, base64 "QUI=" = "AB"
    expect([...encodeField({ number: 1, type: 'bytes', value: 'QUI=' })]).toEqual([0x0a, 0x02, 0x41, 0x42])
  })
})

describe('encodeMessage (Testimonial TestimonialSubmission)', () => {
  it('customer(1)+testimonial(2) 두 string 필드를 이어 붙인다', () => {
    const buf = encodeMessage([
      { number: 1, type: 'string', value: '../x' },
      { number: 2, type: 'string', value: 'hi' }
    ])
    // field1 "../x" (len4) then field2 "hi" (len2)
    expect([...buf]).toEqual([0x0a, 0x04, 0x2e, 0x2e, 0x2f, 0x78, 0x12, 0x02, 0x68, 0x69])
  })
})

describe('frameMessage / unframeMessage', () => {
  it('5바이트 prefix(압축0 + BE length)를 붙이고 되돌린다', () => {
    const msg = Buffer.from([0xaa, 0xbb, 0xcc])
    const framed = frameMessage(msg)
    expect(framed.length).toBe(8)
    expect(framed[0]).toBe(0)
    expect(framed.readUInt32BE(1)).toBe(3)
    expect([...unframeMessage(framed)]).toEqual([0xaa, 0xbb, 0xcc])
  })
  it('5바이트 미만이면 빈 버퍼', () => {
    expect(unframeMessage(Buffer.from([0, 0, 0]))).toHaveLength(0)
  })
})

describe('extractStringFields', () => {
  it('길이지정(wire2) 필드를 UTF-8 로 뽑고 varint 필드는 건너뛴다', () => {
    const buf = encodeMessage([
      { number: 1, type: 'string', value: 'submitted' },
      { number: 2, type: 'int', value: 7 },
      { number: 3, type: 'string', value: 'ok' }
    ])
    const got = extractStringFields(buf)
    expect(got[1]).toBe('submitted')
    expect(got[3]).toBe('ok')
    expect(got[2]).toBeUndefined()
  })
})
