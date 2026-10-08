import { describe, expect, it } from 'vitest'

import { nodeLabel, normalizeListener, snippetAt } from './listeners'

describe('listeners', () => {
  describe('nodeLabel', () => {
    it('태그·id·class로 짧은 라벨을 만든다', () => {
      expect(nodeLabel('BUTTON', ['id', 'login', 'class', 'btn  primary', 'type', 'submit'])).toBe(
        'button#login.btn.primary'
      )
      expect(nodeLabel('DIV', [])).toBe('div')
    })
    it('class는 3개까지만 쓰고 나머지는 …로 줄인다 (Tailwind 대비)', () => {
      expect(nodeLabel('BUTTON', ['class', 'flex w-12 h-12 rounded-full items-center'])).toBe(
        'button.flex.w-12.h-12…'
      )
    })
    it('document·window는 이름 그대로 쓴다', () => {
      expect(nodeLabel('#document', [])).toBe('document')
      expect(nodeLabel('window', [])).toBe('window')
    })
  })

  describe('snippetAt', () => {
    it('여러 줄 소스는 해당 줄부터 maxLines줄을 자른다', () => {
      const src = ['a', 'b', 'function onClick() {', '  send()', '}', 'z'].join('\n')
      expect(snippetAt(src, 2, 0, { maxLines: 3 })).toBe('function onClick() {\n  send()\n}')
    })
    it('한 줄짜리 minified 번들은 컬럼 주변만 잘라 앞뒤에 …를 붙인다', () => {
      const src = 'x'.repeat(5_000) + 'function h(e){sign(e)}' + 'y'.repeat(5_000)
      const s = snippetAt(src, 0, 5_000, { maxChars: 60 })
      expect(s.startsWith('…function h(e){sign(e)}')).toBe(true)
      expect(s.endsWith('…')).toBe(true)
      expect(s.length).toBeLessThanOrEqual(62)
    })
    it('범위를 벗어난 위치는 빈 문자열이다', () => {
      expect(snippetAt('a\nb', 9, 0)).toBe('')
    })
  })

  describe('normalizeListener', () => {
    const raw = {
      type: 'click',
      useCapture: false,
      passive: true,
      once: false,
      scriptId: '42',
      lineNumber: 9,
      columnNumber: 4
    }
    it('0 기반 위치를 1 기반 url:line:col로 바꾸고 붙은 요소를 표시한다', () => {
      const l = normalizeListener(raw, { depth: 0, label: 'button#login' }, 'https://a.io/app.js')
      expect(l).toMatchObject({
        type: 'click',
        on: 'self (button#login)',
        capture: false,
        passive: true,
        once: false,
        location: 'https://a.io/app.js:10:5'
      })
    })
    it('조상 리스너는 몇 단계 위인지, URL이 없으면 scriptId로 표시한다', () => {
      const l = normalizeListener(raw, { depth: 3, label: 'div#root' }, null)
      expect(l.on).toBe('ancestor +3 (div#root)')
      expect(l.location).toBe('script#42:10:5')
    })
  })
})
