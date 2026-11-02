import { describe, it, expect } from 'vitest'

import { serializeTabs, parseSavedTabs, type Tab } from './tabs'

const tab = (id: string, url: string, extra: Partial<Tab> = {}): Tab => ({
  id,
  url,
  title: 'T',
  webContentsId: null,
  initialUrl: url,
  ...extra
})

describe('tabs 세션 복원', () => {
  describe('serializeTabs', () => {
    it('url/title/profileId/partition 을 저장하고 활성 인덱스를 담는다', () => {
      const tabs = [tab('t1', 'https://a.test'), tab('t2', 'https://b.test', { profileId: 'work' })]
      const parsed = JSON.parse(serializeTabs(tabs, 't2'))
      expect(parsed.tabs).toHaveLength(2)
      expect(parsed.tabs[1]).toMatchObject({ url: 'https://b.test', profileId: 'work' })
      expect(parsed.activeIndex).toBe(1)
    })

    it('프록시 탭과 빈 URL 탭은 저장에서 제외한다', () => {
      const tabs = [
        tab('t1', 'https://a.test'),
        tab('t2', 'https://p.test', { proxy: { enabled: true, scheme: 'http', host: 'h', port: 1 } }),
        tab('t3', '')
      ]
      const parsed = JSON.parse(serializeTabs(tabs, 't1'))
      expect(parsed.tabs.map((t: { url: string }) => t.url)).toEqual(['https://a.test'])
    })

    it('활성 탭이 저장 제외 대상이면 인덱스 0 으로 떨어진다', () => {
      const tabs = [
        tab('t1', 'https://a.test'),
        tab('t2', 'https://p.test', { proxy: { enabled: true, scheme: 'http', host: 'h', port: 1 } })
      ]
      expect(JSON.parse(serializeTabs(tabs, 't2')).activeIndex).toBe(0)
    })
  })

  describe('parseSavedTabs', () => {
    it('유효한 저장본을 복원한다', () => {
      const raw = serializeTabs([tab('t1', 'https://a.test')], 't1')
      expect(parseSavedTabs(raw)).toEqual({ tabs: [{ url: 'https://a.test', title: 'T', profileId: undefined, partition: undefined }], activeIndex: 0 })
    })

    it('null·깨진 JSON·빈 탭·url 없는 항목이면 null 을 돌려준다', () => {
      expect(parseSavedTabs(null)).toBeNull()
      expect(parseSavedTabs('not json')).toBeNull()
      expect(parseSavedTabs(JSON.stringify({ tabs: [], activeIndex: 0 }))).toBeNull()
      expect(parseSavedTabs(JSON.stringify({ tabs: [{ title: 'x' }], activeIndex: 0 }))).toBeNull()
    })
  })
})
