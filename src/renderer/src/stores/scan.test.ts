import { describe, it, expect, beforeEach, vi } from 'vitest'

// capture arrays must survive vi.mock hoisting
const state = vi.hoisted(() => ({
  prompts: [] as string[],
  profileCalls: [] as string[],
  tabs: [] as { id: string; url: string; webContentsId: number }[],
  closed: [] as string[],
  tabSeq: 0
}))

// Prompt-driven fake agent: the discovery turn mentions 같은 사이트 링크/경로, the endpoint
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
      } else if (text.includes('같은 사이트 링크/경로')) {
        chunks.push({
          type: 'text-delta',
          delta:
            '["https://example.com/a","https://example.com/b","https://other.com/c","https://example.com/font/Ancient_G_Written.ttf","https://example.com/css/style.css"]'
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
      closeTab: (id: string) => state.closed.push(id),
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

import { useScanStore, cleanReport, scanNotification, mergeQueryEndpoints, workerPrompt, levelLineFor } from './scan'

const TAB_OF = (url: string) => `URL 이 "${url}" 인 탭의 인덱스`

describe('scanNotification', () => {
  it('done 은 발견 수를 담은 완료 알림을 만든다', () => {
    const n = scanNotification('done', 5)
    expect(n.title).toBe('scan.notify.title')
    expect(n.body).toBe('scan.notify.done {"n":5}')
  })

  it('error 는 실패 알림을 만든다', () => {
    expect(scanNotification('error', 0).body).toBe('scan.notify.error')
  })

  it('stopped 는 부분 결과 알림을 만든다', () => {
    expect(scanNotification('stopped', 3).body).toBe('scan.notify.stopped {"n":3}')
  })
})

describe('cleanReport', () => {
  it('런타임 Notice·Model fallback 라인을 제거하고 본문은 남긴다', () => {
    const input =
      '# Reversing Session Findings\n\nNotice: Fable 5.1 safeguards stopped the response above\n- real finding\nModel fallback: claude-fable-5-1 declined this request (cyber)\n- another finding'
    const out = cleanReport(input)
    expect(out).not.toContain('Notice:')
    expect(out).not.toContain('Model fallback:')
    expect(out).toContain('# Reversing Session Findings')
    expect(out).toContain('- real finding')
    expect(out).toContain('- another finding')
  })
})

describe('useScanStore 스캔 파이프라인', () => {
  beforeEach(() => {
    state.prompts.length = 0
    state.profileCalls.length = 0
    state.tabs.length = 0
    state.closed.length = 0
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
    // other.com(사이트 밖) + 폰트/CSS(정적 자산)는 제외 — a/b 두 탭만 연다
    expect(state.tabs.map((t) => t.url)).toEqual(['https://example.com/a', 'https://example.com/b'])
    // the scan closes the tabs it opened once it finishes
    expect(state.closed).toEqual(state.tabs.map((t) => t.id))
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
    // 비-loopback 타겟은 봉인 유지 — 에스컬레이션 없음
    expect(state.prompts.some((p) => p.includes('에스컬레이션 단계'))).toBe(false)
  })

  it('readonly/probe 에서는 숨은 경로(path_probe) 스윕을 돌리지 않는다', async () => {
    useScanStore.getState().setIntensity('probe')
    await useScanStore.getState().start()
    expect(state.prompts.some((p) => p.includes('path_probe({ baseUrl:'))).toBe(false)
  })

  it('일회성 타겟(localhost) redteam 은 에스컬레이션 단계를 돌린다', async () => {
    useScanStore.setState({ target: 'http://localhost:1337/' })
    useScanStore.getState().setIntensity('redteam')
    await useScanStore.getState().start()
    const esc = state.prompts.filter((p) => p.includes('에스컬레이션 단계'))
    expect(esc).toHaveLength(1)
    expect(esc[0]).toContain('최종 임팩트')
    expect(esc[0]).toContain('tags:["impact"]')
    expect(state.profileCalls).toEqual(['reverse', 'probe', 'reverse'])
    expect(useScanStore.getState().phase).toBe('done')
  })
})

describe('mergeQueryEndpoints', () => {
  it('쿼리 있는 링크가 베어 GET 을 대체한다 — 주입 계열 체크리스트가 살아난다', () => {
    expect(
      mergeQueryEndpoints(['GET http://localhost:1337/'], ['http://localhost:1337/?format=%H'])
    ).toEqual(['GET http://localhost:1337/?format=%H'])
  })

  it('POST 엔드포인트는 유지하고 쿼리 GET 을 추가한다 — Labyrinth 형태', () => {
    expect(mergeQueryEndpoints(['POST http://x/'], ['http://x/?a=1'])).toEqual([
      'POST http://x/',
      'GET http://x/?a=1'
    ])
  })

  it('에이전트가 이미 쿼리 URL 을 냈으면 중복 추가하지 않는다', () => {
    expect(
      mergeQueryEndpoints(['GET http://x/?format=1'], ['http://x/?format=2'])
    ).toEqual(['GET http://x/?format=1'])
  })

  it('쿼리 없는 링크만 있으면 그대로다', () => {
    expect(mergeQueryEndpoints(['GET http://x/'], ['http://x/about'])).toEqual(['GET http://x/'])
  })
})

describe('workerPrompt — 봉인 조건부화', () => {
  it('실제 타겟(비-loopback)은 파괴적 컨트롤 금지가 그대로다', () => {
    const p = workerPrompt('https://example.com/a', false, false)
    expect(p).toContain('buy/pay/delete/결제/삭제/제출 확인 컨트롤 클릭 금지')
    expect(p).toContain('로그인 페이지는 기록만')
    expect(p).not.toContain('일회성')
  })

  it('일회성 타겟(aggressive)은 금지가 풀리고 모든 조작이 허용된다', () => {
    const p = workerPrompt('http://localhost:1337/a', true, true)
    expect(p).not.toContain('클릭 금지. 로그인 페이지는 기록만')
    expect(p).toContain('제출·삭제·결제 컨트롤을 포함한 모든 조작이 허용')
    // 공통 가드는 유지 — rever 도구만, 다른 탭 금지
    expect(p).toContain('Terminal/Bash/셸/curl/wget 및 ToolSearch 금지')
    expect(p).toContain('다른 탭 열기/닫기/선택 금지')
  })
})

describe('levelLineFor — 강도 상한', () => {
  it('probe 는 변경 없는 재생만', () => {
    expect(levelLineFor('probe', true)).toContain('변경 없이 재생만')
    expect(levelLineFor('probe', true)).toContain('금지')
  })

  it('실제 타겟 redteam 은 변형 1개 상한을 유지한다', () => {
    const line = levelLineFor('redteam', false)
    expect(line).toContain('변형 요청 최대 1개')
    expect(line).not.toContain('적응 재시도')
  })

  it('일회성 타겟 redteam 은 응답 기반 적응 재시도 최대 5회를 허용한다', () => {
    const line = levelLineFor('redteam', true)
    expect(line).toContain('최대 5회까지 적응 재시도')
    expect(line).toContain('워드리스트·대량 열거·레이트리밋 재시도는 여전히 금지')
  })
})
