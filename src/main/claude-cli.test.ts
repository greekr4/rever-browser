import { describe, expect, it } from 'vitest'
import { pickClaudeCli } from './claude-cli'

describe('pickClaudeCli', () => {
  it('디렉터리 순서대로 첫 번째 실행 가능한 claude를 고른다', () => {
    const exists = (p: string) => p === '/home/u/.local/bin/claude' || p === '/usr/local/bin/claude'
    expect(pickClaudeCli(['/opt/homebrew/bin', '/home/u/.local/bin', '/usr/local/bin'], exists, false)).toBe(
      '/home/u/.local/bin/claude'
    )
  })
  it('없으면 null을 돌려준다', () => {
    expect(pickClaudeCli(['/a', '/b'], () => false, false)).toBeNull()
  })
  it('빈 디렉터리 항목은 건너뛴다', () => {
    const seen: string[] = []
    pickClaudeCli(['', '/a'], (p) => (seen.push(p), false), false)
    expect(seen).toEqual(['/a/claude'])
  })
  it('Windows에서는 .exe/.cmd 변형도 찾는다', () => {
    const exists = (p: string) => p.endsWith('claude.cmd')
    expect(pickClaudeCli(['C:\\npm'], exists, true)).toMatch(/claude\.cmd$/)
  })
})
