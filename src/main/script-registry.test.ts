import { afterEach, describe, expect, it } from 'vitest'

import {
  __resetScriptRegistryForTests,
  clearScriptsFor,
  recordScript,
  scriptUrl
} from './script-registry'

afterEach(() => __resetScriptRegistryForTests())

describe('script-registry', () => {
  it('탭·세션·scriptId 조합으로 URL을 기록하고 찾는다', () => {
    recordScript(1, undefined, '42', 'https://a.io/app.js')
    recordScript(1, 'oopif-1', '42', 'https://b.io/frame.js')
    recordScript(2, undefined, '42', 'https://c.io/other.js')
    expect(scriptUrl(1, undefined, '42')).toBe('https://a.io/app.js')
    expect(scriptUrl(1, 'oopif-1', '42')).toBe('https://b.io/frame.js')
    expect(scriptUrl(2, undefined, '42')).toBe('https://c.io/other.js')
    expect(scriptUrl(1, undefined, '99')).toBeNull()
  })

  it('URL이 빈 인라인/eval 스크립트는 null로 돌려준다', () => {
    recordScript(1, undefined, '7', '')
    expect(scriptUrl(1, undefined, '7')).toBeNull()
  })

  it('상한을 넘으면 가장 오래된 기록부터 지운다', () => {
    for (let i = 0; i < 20_001; i++) recordScript(1, undefined, String(i), `u${i}`)
    expect(scriptUrl(1, undefined, '0')).toBeNull()
    expect(scriptUrl(1, undefined, '20000')).toBe('u20000')
  })

  it('탭이 닫히면 그 탭의 기록만 지운다', () => {
    recordScript(1, undefined, '1', 'a')
    recordScript(2, undefined, '1', 'b')
    clearScriptsFor(1)
    expect(scriptUrl(1, undefined, '1')).toBeNull()
    expect(scriptUrl(2, undefined, '1')).toBe('b')
  })
})
