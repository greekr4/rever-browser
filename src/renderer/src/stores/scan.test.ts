import { describe, it, expect, beforeEach, vi } from 'vitest'

// capture arrays must survive vi.mock hoisting
const state = vi.hoisted(() => ({
  prompts: [] as string[],
  profileCalls: [] as string[],
  tabs: [] as { id: string; url: string; webContentsId: number }[],
  tabSeq: 0
}))

// Prompt-driven fake agent: the discovery turn mentions 깊이 2, the endpoint
// collector 중복 없이, the export turn finding_export; everything else is a
// worker/probe turn that records one finding.
vi.mock('@/ai/acp-transport', () => ({
  ACPChatTransport: class {
    async sendMessages({ messages }: { messages: { parts: { text: string }[] }[] }) {
      const text = messages[0].parts[0].text
      state.prompts.push(text)
      const chunks: Record<string, unknown>[] = []
      if (text.includes('finding_export')) {
        chunks.push(
          { type: 'text-delta', delta: 'report:' },
          {
            type: 'tool-output-available',
            output: [{ type: 'text', text: 'Reversing Session Findings\n| merged |' }]
          }
        )
      } else if (text.includes('중복 없이')) {
        chunks.push({
          type: 'text-delta',
          delta: '["GET https://example.com/api/x","GET https://example.com/api/x?a=1"]'
        })
      } else if (text.includes('깊이 2')) {
        chunks.push({
          type: 'text-delta',
          delta: '["https://example.com/a","https://example.com/b","https://other.com/c"]'
        })
      } else {
        chunks.push(
          { type: 'tool-input-start', toolName: 'finding_add' },
          { type: 'text-delta', delta: '발견 1건' }
        )
      }
      return new ReadableStream({
        start(ctrl) {
          for (const c of chunks) ctrl.enqueue(c)
          ctrl.close()
        }
      })
    }
    async reset() {}
  }
}))

vi.mock('./tool-profile', () => ({
  useToolProfileStore: {
    getState: () => ({
      setProfile: (p: string) => {
        state.profileCalls.push(p)
        return Promise.resolve()
      }
    })
  }
}))

vi.mock('./tabs', () => ({
  useTabsStore: {
    getState: () => ({
      addTab: (url: string) => {
        const id = `t${++state.tabSeq}`
        state.tabs.push({ id, url, webContentsId: state.tabSeq })
        return id
      },
      tabs: state.tabs
    })
  }
}))

vi.mock('./i18n', () => ({
  useI18nStore: { getState: () => ({ lang: 'en' }) },
  t: (key: string, vars?: Record<string, string | number>) =>
    vars ? `${key} ${JSON.stringify(vars)}` : key
}))

vi.mock('@/constants', () => ({ ACP_AGENTS: [{ id: 'claude-code' }] }))

import { useScanStore } from './scan'

const TAB_OF = (url: string) => `URL 이 "${url}" 인 탭의 인덱스`

describe('useScanStore 스캔 파이프라인', () => {
  beforeEach(() => {
    state.prompts.length = 0
    state.profileCalls.length = 0
    state.tabs.length = 0
    state.tabSeq = 0
    useScanStore.setState({
      phase: 'idle',
      target: 'https://example.com/',
      pages: 5,
      intensity: 'readonly',
      scanTabs: {},
      findings: 0,
      report: '',
      errorText: ''
    })
  })

  it('readonly 에서는 probe 프로필로 한 번도 전환하지 않는다', async () => {
    await useScanStore.getState().start()
    expect(state.profileCalls).toEqual(['reverse'])
    expect(useScanStore.getState().phase).toBe('done')
    expect(useScanStore.getState().report).toContain('Reversing Session Findings')
  })

  it('같은 사이트 링크만 탭으로 열고 워커 프롬프트에 그 탭 URL 이 있다', async () => {
    await useScanStore.getState().start()
    // other.com 링크는 제외 — a/b 두 탭만 연다
    expect(state.tabs.map((t) => t.url)).toEqual(['https://example.com/a', 'https://example.com/b'])
    const workerPrompts = state.prompts.filter((p) => p.includes('인 탭의 인덱스'))
    expect(workerPrompts).toHaveLength(2)
    expect(workerPrompts.some((p) => p.includes(TAB_OF('https://example.com/a')))).toBe(true)
    expect(workerPrompts.some((p) => p.includes(TAB_OF('https://example.com/b')))).toBe(true)
    for (const p of workerPrompts) {
      expect(p).toContain('browser_snapshot({tab:N})')
      expect(p).toContain('금지')
    }
  })

  it('redteam 에서는 probe 프로필로 갔다가 reverse 로 돌아오고 중복 엔드포인트는 한 번만 프로브한다', async () => {
    useScanStore.getState().setIntensity('redteam')
    await useScanStore.getState().start()
    expect(state.profileCalls).toEqual(['reverse', 'probe', 'reverse'])
    // collector 출력의 두 중복(path 동일, 쿼리만 다름)은 1개로 dedup → 프로브 턴 1개
    const probePrompts = state.prompts.filter((p) => p.includes('강도: redteam'))
    expect(probePrompts).toHaveLength(1)
    expect(probePrompts[0]).toContain('https://example.com/api/x')
    expect(probePrompts[0]).toContain('Write every finding body in English.')
    // Scanner: a deterministic hidden-path sweep on the origin runs at redteam.
    const hidden = state.prompts.filter((p) => p.includes('path_probe({ baseUrl:'))
    expect(hidden).toHaveLength(1)
    expect(hidden[0]).toContain('https://example.com')
  })

  it('readonly/probe 에서는 숨은 경로(path_probe) 스윕을 돌리지 않는다', async () => {
    useScanStore.getState().setIntensity('probe')
    await useScanStore.getState().start()
    expect(state.prompts.some((p) => p.includes('path_probe({ baseUrl:'))).toBe(false)
  })
})
