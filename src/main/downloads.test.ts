import { describe, expect, it, vi } from 'vitest'

vi.mock('electron', () => ({ app: { getPath: () => '/tmp' }, BrowserWindow: { getAllWindows: () => [] } }))

import { uniqueFilename } from './downloads'

describe('uniqueFilename', () => {
  it('겹치지 않으면 이름을 그대로 쓴다', () => {
    expect(uniqueFilename('a.pdf', () => false)).toBe('a.pdf')
  })

  it('겹치면 확장자 앞에 번호를 붙인다', () => {
    const taken = new Set(['a.pdf', 'a (1).pdf'])
    expect(uniqueFilename('a.pdf', (n) => taken.has(n))).toBe('a (2).pdf')
  })

  it('확장자가 없거나 점으로 시작하는 이름도 처리한다', () => {
    expect(uniqueFilename('README', (n) => n === 'README')).toBe('README (1)')
    expect(uniqueFilename('.env', (n) => n === '.env')).toBe('.env (1)')
  })
})
