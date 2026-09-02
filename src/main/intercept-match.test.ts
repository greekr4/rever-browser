import { describe, it, expect } from 'vitest'

import { compileUrlPattern, compileRules, findMatchingRule } from './intercept-match'

describe('compileUrlPattern', () => {
  it('treats * as any run of characters and anchors the match', () => {
    const re = compileUrlPattern('*/api/v1/*')
    expect(re.test('https://x.com/api/v1/users')).toBe(true)
    expect(re.test('https://x.com/api/v2/users')).toBe(false)
  })

  it('treats ? as exactly one character', () => {
    const re = compileUrlPattern('https://x.com/item?')
    expect(re.test('https://x.com/item1')).toBe(true)
    expect(re.test('https://x.com/item12')).toBe(false)
  })

  it('does not throw on unbalanced regex metacharacters', () => {
    expect(() => compileUrlPattern('*/api/(v1/*')).not.toThrow()
    expect(compileUrlPattern('*/api/(v1/*').test('https://x.com/api/(v1/z')).toBe(true)
    expect(compileUrlPattern('*/api/(v1/*').test('https://x.com/api/v1/z')).toBe(false)
  })

  it('treats a dot literally', () => {
    expect(compileUrlPattern('https://x.com/*').test('https://xzcom/')).toBe(false)
  })

  it('honours backslash escapes', () => {
    const re = compileUrlPattern('https://x.com/a\\*b')
    expect(re.test('https://x.com/a*b')).toBe(true)
    expect(re.test('https://x.com/aXb')).toBe(false)
  })
})

describe('findMatchingRule', () => {
  it('returns the first matching rule in order', () => {
    const compiled = compileRules([
      { id: 'a', urlPattern: '*/other/*' },
      { id: 'b', urlPattern: '*/api/*' },
      { id: 'c', urlPattern: '*' }
    ])
    expect(findMatchingRule(compiled, 'https://x.com/api/1')?.id).toBe('b')
    expect(findMatchingRule(compiled, 'https://x.com/zzz')?.id).toBe('c')
    expect(findMatchingRule([], 'https://x.com/zzz')).toBeUndefined()
  })
})
