import { describe, expect, it } from 'vitest'

import { parseActionable, endpointSig } from './ui-crawl'

describe('ui_crawl helpers', () => {
  describe('parseActionable', () => {
    it('ref 가 붙은 행만 뽑고 라벨로 중복을 제거한다', () => {
      const tree = [
        '- document "App"',
        '  - button "Load results" [ref=r3]',
        '  - link "Docs" [ref=r4]',
        '  - text "just a label"',
        '  - button "Load results" [ref=r9]' // same label, different ref → one entry
      ].join('\n')
      const out = parseActionable(tree)
      expect(out.map((a) => a.ref)).toEqual(['r3', 'r4'])
      expect(out.map((a) => a.label)).toEqual(['button "Load results"', 'link "Docs"'])
    })

    it('ref 없는 트리는 빈 배열', () => {
      expect(parseActionable('- document "x"\n  - text "y"')).toEqual([])
    })

    it('라벨이 비면 ref 를 키로 쓴다', () => {
      const out = parseActionable('  - [ref=r1]')
      expect(out).toHaveLength(1)
      expect(out[0].key).toBe('r1')
    })
  })

  describe('endpointSig', () => {
    it('메서드를 대문자화하고 쿼리/프래그먼트를 제거한다', () => {
      expect(endpointSig('get', 'https://x.test/api/items?page=2#frag')).toBe('GET https://x.test/api/items')
      expect(endpointSig('POST', 'https://x.test/api/items')).toBe('POST https://x.test/api/items')
    })

    it('파싱 불가 URL 도 쿼리만 떼고 보존한다', () => {
      expect(endpointSig('GET', 'not a url?x=1')).toBe('GET not a url')
    })
  })
})
