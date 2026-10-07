import { afterEach, describe, expect, it } from 'vitest'

import { __resetResultStoreForTests, budgetText, getResultSlice, okBudgeted } from './utils'

afterEach(() => __resetResultStoreForTests())

describe('utils budgeting', () => {
  describe('budgetText', () => {
    it('예산 이하면 그대로 둔다', () => {
      expect(budgetText('short', 100)).toEqual({ head: 'short', truncated: false, shownChars: 5 })
    })
    it('예산 초과면 줄 경계로 자른다', () => {
      const text = 'line1\nline2\nline3\nline4'
      const r = budgetText(text, 14) // falls inside line3
      expect(r.truncated).toBe(true)
      expect(r.head).toBe('line1\nline2')
      expect(r.shownChars).toBe(11)
    })
    it('가까운 줄바꿈이 없으면 하드 컷한다', () => {
      const text = 'x'.repeat(100)
      const r = budgetText(text, 10)
      expect(r.head.length).toBe(10)
      expect(r.truncated).toBe(true)
    })
  })

  describe('okBudgeted + getResultSlice', () => {
    it('짧은 결과는 꼬리표 없이 그대로 반환한다', () => {
      const res = okBudgeted('hello', { maxChars: 100 })
      expect(res.content[0].text).toBe('hello')
    })

    it('긴 결과는 잘리고 resultId로 이어볼 수 있다', () => {
      const text = Array.from({ length: 50 }, (_, i) => `row-${i}`).join('\n')
      const res = okBudgeted(text, { maxChars: 40 })
      const shown = res.content[0].text
      expect(shown).toContain('truncated')
      const idMatch = /resultId="(res_\d+)"/.exec(shown)
      const offMatch = /offset=(\d+)/.exec(shown)
      expect(idMatch).not.toBeNull()
      const id = idMatch![1]
      const offset = Number(offMatch![1])

      const next = getResultSlice(id, offset, 40)
      expect(next).not.toBeNull()
      expect(next!.total).toBe(text.length)
      // head + next slice reconstructs the original prefix boundary
      expect(next!.offset).toBe(offset)
    })

    it('끝까지 페이지하면 more=false, nextOffset=null', () => {
      const text = 'y'.repeat(100)
      okBudgeted(text, { maxChars: 30 })
      // the store id is res_1 for the first stored result
      const whole = getResultSlice('res_1', 0, 1000)
      expect(whole).not.toBeNull()
      expect(whole!.slice.length).toBe(100)
      expect(whole!.more).toBe(false)
      expect(whole!.nextOffset).toBeNull()
    })

    it('없는 resultId는 null', () => {
      expect(getResultSlice('res_999', 0)).toBeNull()
    })
  })
})
