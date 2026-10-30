import { create } from 'zustand'

import type { UIMessage, UIMessageChunk } from 'ai'

import { ACPChatTransport } from '@/ai/acp-transport'
import { checklistFor } from '@/ai/redteam-categories'
import { addRedTeamScope, setRedTeamScan, siteKey } from '@/ai/red-team-mode'
import REDTEAM_PROBE_PROMPT from '@/ai/redteam-probe.md?raw'
import { ACP_AGENTS } from '@/constants'

import { t, useI18nStore } from './i18n'
import { useTabsStore } from './tabs'
import { useToolProfileStore } from './tool-profile'
import { useTrafficStore } from './traffic'

// Hosts the target app actually talked to (captured traffic) that share its
// bare host — i.e. the same app's API on another localhost port, or a sibling
// subdomain. Unrelated apps never appear unless the app called them.
function connectedHosts(targetKey: string): string[] {
  const bare = targetKey.split(':')[0]
  const out = new Set<string>()
  for (const e of Object.values(useTrafficStore.getState().entries)) {
    const k = siteKey(e.url)
    if (k && k !== targetKey && k.split(':')[0] === bare) out.add(k)
  }
  return [...out]
}

type Phase = 'idle' | 'confirm' | 'running' | 'done' | 'error'
type TabStatus = 'opening' | 'inspecting' | 'done' | 'error'
/**
 * Sweep intensity, set in the popup.
 * readonly — observe and record only (no probe tools).
 * probe — replay captured requests unchanged + security_inspect + scan_secrets.
 * redteam — plus single-shot mutated probes (payload/path/crlf). Load tools
 * (intruder/burst) are manual at every level.
 */
export type ScanIntensity = 'readonly' | 'probe' | 'redteam'

// Per-tab inspection status, keyed by the tabs-store tab id, so each grid tile
// can show what its sub-agent is doing.
export interface ScanTab {
  url: string
  status: TabStatus
  findings: number
}

// One line in the terminal-style activity log the popup renders, so the user
// can watch what the scan is doing (tool calls, findings) as it runs.
export type LogKind = 'phase' | 'tool' | 'finding' | 'error'
export interface LogEntry {
  ts: number
  kind: LogKind
  text: string
  tabId?: string
}
const LOG_CAP = 300

// Scan state lives in the store, not the popup, so closing the movable popup
// mid-scan keeps the run going and its report intact — reopening shows it live.
interface ScanState {
  visible: boolean
  phase: Phase
  target: string
  pages: number
  pagesSeen: number
  findings: number
  currentTool: string
  report: string
  errorText: string
  startedAt: number
  scanTabs: Record<string, ScanTab>
  log: LogEntry[]
  pushLog: (kind: LogKind, text: string, tabId?: string) => void
  /** Sweep intensity — see ScanIntensity. */
  intensity: ScanIntensity
  setIntensity: (m: ScanIntensity) => void
  open: (url?: string) => void
  close: () => void
  setTarget: (t: string) => void
  setPages: (n: number) => void
  askConfirm: () => void
  cancel: () => void
  start: () => Promise<void>
  stop: () => void
}

// The main (orchestrator) transport, kept out of the store so React never tries
// to diff it. Per-tab worker transports are created and torn down in phase 3.
let transport: ACPChatTransport | null = null
let abort: AbortController | null = null
const workers: ACPChatTransport[] = []
// Run open tabs / endpoints in parallel, but cap it: each worker spawns a
// claude-agent-acp session whose ~10s cold boot times out under a spawn storm
// (20+ at once → "ACP newSession timed out"). 8 keeps it fast without the storm.
const MAX_PARALLEL = 8
// Set by stop() so start()'s catch finalizes (exports what was found) instead
// of discarding — findings are already persisted by finding_add as it runs.
let stopRequested = false

// A live event from an agent turn, fed to the activity log + counters.
type ToolEvent =
  | { kind: 'tool'; name: string }
  | { kind: 'finding'; input: unknown }
  | { kind: 'scope'; input: unknown }

// Hosts the agent brought into scope via scope_set/scope_add.
function scopeHostsOf(input: unknown): string[] {
  if (input && typeof input === 'object') {
    const h = (input as Record<string, unknown>).hosts
    if (Array.isArray(h)) return h.filter((x): x is string => typeof x === 'string')
  }
  return []
}

// MCP tools arrive namespaced (mcp__rever__browser_navigate) — show the bare id.
function stripTool(name: string): string {
  return name.split('__').at(-1) || name
}

// Best-effort one-line summary of a finding_add call for the activity log.
function describeFinding(input: unknown): string {
  if (!input || typeof input !== 'object') return t('scan.log.finding')
  const o = input as Record<string, unknown>
  const f = o.finding && typeof o.finding === 'object' ? (o.finding as Record<string, unknown>) : o
  const pick = (k: string): string | undefined => (typeof f[k] === 'string' ? (f[k] as string) : undefined)
  const title = pick('title') ?? pick('name') ?? pick('summary') ?? pick('kind') ?? pick('type') ?? t('scan.log.finding')
  const loc = pick('url') ?? pick('endpoint') ?? pick('path') ?? ''
  return loc ? `${title} — ${loc}` : title
}

// Run one agent turn on a given transport; returns its text, any export report,
// and how many finding_add calls it made. onEvent fires for live UI (log +
// counters): one 'tool' per tool call, one 'finding' per finding_add.
async function runTurnOn(
  t: ACPChatTransport,
  text: string,
  signal: AbortSignal,
  onEvent?: (e: ToolEvent) => void
): Promise<{ finalText: string; exportText: string; findingAdds: number }> {
  const messages = [{ id: 'scan', role: 'user', parts: [{ type: 'text', text }] }] as unknown as UIMessage[]
  const stream = (await t.sendMessages({
    messages,
    abortSignal: signal,
    trigger: 'submit-message',
    chatId: 'scan'
  } as never)) as ReadableStream<UIMessageChunk>
  let finalText = ''
  let exportText = ''
  let findingAdds = 0
  const reader = stream.getReader()
  for (;;) {
    const { value, done } = await reader.read()
    if (done) break
    const c = value as UIMessageChunk & { toolName?: string; delta?: string; output?: unknown; errorText?: string }
    if (c.type === 'tool-input-start') {
      const name = c.toolName ?? ''
      onEvent?.({ kind: 'tool', name })
      if (name.includes('finding_add')) findingAdds++
    } else if (c.type === 'tool-input-available') {
      const ci = c as UIMessageChunk & { toolName?: string; input?: unknown }
      const name = ci.toolName ?? ''
      if (name.includes('finding_add')) onEvent?.({ kind: 'finding', input: ci.input })
      else if (name.includes('scope_set') || name.includes('scope_add'))
        onEvent?.({ kind: 'scope', input: ci.input })
    } else if (c.type === 'tool-output-available') {
      const out = extractText(c.output)
      if (out.includes('Reversing Session Findings')) exportText = out
    } else if (c.type === 'text-delta') {
      finalText += c.delta ?? ''
    } else if (c.type === 'error') {
      throw new Error(c.errorText || 'scan failed')
    }
  }
  return { finalText, exportText, findingAdds }
}

// Run `fn` over `items` with at most `limit` in flight at once.
async function pool<T>(items: T[], limit: number, fn: (item: T) => Promise<void>): Promise<void> {
  let i = 0
  const run = async (): Promise<void> => {
    while (i < items.length) {
      const item = items[i++]
      await fn(item)
    }
  }
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, run))
}

// Tool output arrives as rawOutput: a string, a content part, or an array of
// them ([{type:"text",text}]). Pull the text out so the report renders as
// Markdown, not raw JSON with escaped newlines.
function extractText(output: unknown): string {
  if (output == null) return ''
  if (typeof output === 'string') {
    const t = output.trim()
    if (t.startsWith('[') || t.startsWith('{')) {
      try {
        return extractText(JSON.parse(t))
      } catch {
        return output
      }
    }
    return output
  }
  if (Array.isArray(output)) return output.map(extractText).join('')
  if (typeof output === 'object') {
    const o = output as Record<string, unknown>
    if (typeof o.text === 'string') return o.text
  }
  return ''
}

// Wait until every scan tab's webview has attached (webContentsId assigned on
// dom-ready). Background tabs that never finish loading are skipped after the
// timeout rather than blocking the pool.
async function waitForTabs(tabIds: string[], timeoutMs = 15000): Promise<void> {
  const start = Date.now()
  for (;;) {
    const tabs = useTabsStore.getState().tabs
    const ready = tabIds.every((id) => tabs.find((t) => t.id === id)?.webContentsId != null)
    if (ready || Date.now() - start > timeoutMs) return
    await new Promise((r) => setTimeout(r, 200))
  }
}

// Findings language follows the app language so a report never mixes tongues
// across workers.
function findingsLangLine(): string {
  const lang = useI18nStore.getState().lang
  return lang === 'ko'
    ? 'finding 본문은 모두 한국어로 작성한다.'
    : 'Write every finding body in English.'
}

// The per-tab inspection turn. The worker may DRIVE its own tab (tab-param
// tools are race-free — refs live in that tab's namespace), which is what
// makes dynamic exploration (accordions, pagination, XHR triggers) possible.
function workerPrompt(url: string): string {
  return (
    `여러 탭이 열려 있다. browser_tabs({action:"list"}) 로 목록을 보고 URL 이 "${url}" 인 탭의 인덱스 N 을 찾는다. 그 탭만 다룬다:\n` +
    `- 읽기: list_endpoints({tab:N}), list_requests({tab:N}), 흥미로운 요청은 get_request/get_request_initiator, security_inspect({url:"${url}"})\n` +
    `- 동적 탐색(필요할 때): browser_snapshot({tab:N}) → browser_click/browser_type/browser_navigate 에 같은 tab:N 을 전달한다(아코디언 펼치기, 페이지네이션, 더보기, 검색). ref 는 탭별 네임스페이스라 같은 tab:N 으로만 동작한다.\n` +
    `- 오직 rever 도구만 사용한다. Terminal/Bash/셸/curl/wget 및 ToolSearch 금지(scope 를 우회한다). 다른 탭 열기/닫기/선택 금지. buy/pay/delete/결제/삭제/제출 확인 컨트롤 클릭 금지. 로그인 페이지는 기록만.\n` +
    `- finding_add 는 소형 템플릿: 한 줄 제목(엔드포인트/이슈), 관측 불릿 3개 이하, 재현 curl 한 줄. 이미 finding_list 에 같은 엔드포인트가 있으면 추가하지 않는다. ${findingsLangLine()}\n` +
    `- 끝나면 발견 개수만 한 줄로 답한다.`
  )
}

// Pull the JSON array of same-site links the discovery turn prints.
function parseLinks(text: string, site: string, cap: number): string[] {
  const m = text.match(/\[[\s\S]*\]/)
  if (!m) return []
  let arr: unknown
  try {
    arr = JSON.parse(m[0])
  } catch {
    return []
  }
  if (!Array.isArray(arr)) return []
  const out: string[] = []
  const seen = new Set<string>()
  for (const x of arr) {
    if (typeof x !== 'string' || !/^https?:\/\//i.test(x)) continue
    if (siteKey(x) !== site || seen.has(x)) continue
    seen.add(x)
    out.push(x)
    if (out.length >= cap) break
  }
  return out
}

// Same, for "METHOD URL" endpoint strings the redteam collector turn prints;
// the method prefix must not break the same-site check.
function parseEndpoints(text: string, site: string, cap: number): string[] {
  const m = text.match(/\[[\s\S]*\]/)
  if (!m) return []
  let arr: unknown
  try {
    arr = JSON.parse(m[0])
  } catch {
    return []
  }
  if (!Array.isArray(arr)) return []
  const out: string[] = []
  const seen = new Set<string>()
  for (const x of arr) {
    if (typeof x !== 'string') continue
    const ep = /^([A-Z]+)\s+(https?:\/\/\S+)$/i.exec(x.trim())
    const url = ep ? ep[2] : x.trim()
    if (!/^https?:\/\//i.test(url)) continue
    if (siteKey(url) !== site) continue
    const key = url.replace(/[?#].*$/, '')
    if (seen.has(key)) continue
    seen.add(key)
    out.push(ep ? `${ep[1].toUpperCase()} ${url}` : `GET ${url}`)
    if (out.length >= cap) break
  }
  return out
}

export const useScanStore = create<ScanState>((set, get) => ({
  visible: false,
  phase: 'idle',
  target: '',
  pages: 20,
  pagesSeen: 0,
  findings: 0,
  currentTool: '',
  report: '',
  errorText: '',
  startedAt: 0,
  scanTabs: {},
  log: [],
  pushLog: (kind, text, tabId) =>
    set((st) => ({ log: [...st.log, { ts: Date.now(), kind, text, tabId }].slice(-LOG_CAP) })),
  intensity: 'readonly',
  setIntensity: (m) => set({ intensity: m }),
  open: (url) => set((st) => ({ visible: true, target: url || st.target })),
  close: () => set({ visible: false }),
  setTarget: (t) => set({ target: t }),
  setPages: (n) => set({ pages: Math.max(1, Math.min(200, n || 1)) }),
  askConfirm: () => set({ phase: 'confirm' }),
  cancel: () => set({ phase: 'idle' }),
  stop: () => {
    // Don't discard: flag the run so start()'s catch exports what was found.
    stopRequested = true
    for (const w of workers.splice(0)) void w.reset()
    get().pushLog('phase', t('scan.log.stopped'))
    set({ currentTool: '중지 — 지금까지 발견으로 보고서 작성 중…' })
    try {
      abort?.abort()
    } catch {}
  },
  start: async () => {
    const { target, pages, intensity } = get()
    if (!target.trim()) return
    set({ phase: 'running', pagesSeen: 0, findings: 0, currentTool: '', report: '', errorText: '', startedAt: Date.now(), scanTabs: {}, log: [] })
    const { pushLog } = get()
    // Feed the activity log + the status line from each agent turn. tabId is
    // passed by the per-tab workers so the log can show which tab acted.
    const onEv = (e: ToolEvent, tabId?: string): void => {
      if (e.kind === 'tool') {
        const n = stripTool(e.name)
        set({ currentTool: n })
        pushLog('tool', n, tabId)
      } else if (e.kind === 'finding') {
        pushLog('finding', describeFinding(e.input), tabId)
      } else {
        // Track scope the agent set/added so auto-approve matches it (e.g. the
        // app's own API on another localhost port).
        addRedTeamScope(scopeHostsOf(e.input))
      }
    }
    const newAgent = (): ACPChatTransport =>
      new ACPChatTransport({ agentDef: ACP_AGENTS.find((a) => a.id === 'claude-code')! })
    try {
      // Phases 1–3 and the export need the reverse toolset; the probe phase
      // (probe/redteam intensity) switches to probe — which only this
      // user-confirmed flow may do; agent-side tool_profile_set rejects it.
      await useToolProfileStore.getState().setProfile('reverse')
      if (!transport) transport = newAgent()
      await transport.reset()
      abort = new AbortController()
      const signal = abort.signal
      const site = siteKey(target)
      // Pin scope to the exact host:port so the server-side scope check is
      // port-specific too (localhost:8080 must not cover :8090).
      const scopeArg = (() => {
        try {
          return new URL(target).host
        } catch {
          return target.trim()
        }
      })()
      const origin = (() => {
        try {
          return new URL(target).origin
        } catch {
          return target.trim()
        }
      })()
      // While the scan runs, probe tools on this site auto-approve (user chose
      // "allow all" for Red Team). Cleared in finally so nothing slips through
      // after the sweep ends.
      setRedTeamScan(true, target)
      const setTab = (tabId: string, patch: Partial<ScanTab>): void =>
        set((st) => ({ scanTabs: { ...st.scanTabs, [tabId]: { ...st.scanTabs[tabId], ...patch } } }))

      const jobs: { url: string; tabId: string }[] = []
      const scanTabs: Record<string, ScanTab> = {}

      // Phase 1 — discovery: the agent walks the start page AND follows a few
      // links (depth 2) to gather a broad same-site link set.
      pushLog('phase', t('scan.log.discover', { site }))
      const { finalText: linksText } = await runTurnOn(
        transport,
        `scope_set({ hosts: ["${scopeArg}"] }) 로 범위를 고정하고 browser_navigate("${target.trim()}") 후 browser_snapshot. list_requests 로 이 앱이 호출하는 자체 API 호스트(예: 다른 localhost 포트의 백엔드, api.* 서브도메인)를 확인하면 scope_add 로 범위에 추가한다 — 단 서드파티(분석/광고/CDN)는 추가하지 않는다. list_endpoints 가 없으면 tool_profile_set({ profile: "reverse" }) 먼저. 같은 사이트 링크를 모으되, 대표 링크 2~3개는 browser_navigate 로 들어가 browser_snapshot 해서 그 안의 링크까지(깊이 2) 수집한다. 점검할 만한 페이지를 최대 ${pages}개. 설명 없이 절대경로 URL들의 JSON 배열 한 줄만 출력.`,
        signal,
        (e) => onEv(e)
      )
      const links = parseLinks(linksText, site, pages)

      // Phase 2 — the STORE opens them as background tabs (deterministic
      // parallel open; they tile in the grid) and waits for attach.
      for (const url of links) {
        try {
          const tabId = useTabsStore.getState().addTab(url, { activate: false })
          jobs.push({ url, tabId })
          scanTabs[tabId] = { url, status: 'opening', findings: 0 }
        } catch {}
      }
      set({ pagesSeen: links.length, scanTabs })
      pushLog('phase', t('scan.log.pagesFound', { n: links.length }))
      await waitForTabs(jobs.map((j) => j.tabId))
      await new Promise((r) => setTimeout(r, 2000)) // settle: let capture fill

      // Follow the connections: pull every host the app actually talked to
      // (its API on another port, sibling subdomains) into scope — deterministic,
      // not left to the agent. Unrelated apps never appear unless the app calls
      // them. scope_add on the server + the renderer's auto-approve scope.
      const connected = connectedHosts(site)
      if (connected.length) {
        addRedTeamScope(connected)
        pushLog('phase', t('scan.log.scopeExpand', { hosts: connected.join(', ') }))
        await runTurnOn(
          transport,
          `scope_add({ hosts: ${JSON.stringify(connected)} }) 로 이 앱이 연결된 호스트를 범위에 추가한다. 다른 설명 없이 호출만.`,
          signal,
          (e) => onEv(e)
        )
      }

      // Phase 3 — one worker agent per tab, reading and exploring in parallel
      // (all at once, up to the process cap).
      const tabLimit = Math.min(jobs.length, MAX_PARALLEL)
      pushLog('phase', t('scan.log.inspecting', { n: tabLimit }))
      await pool(jobs, tabLimit, async ({ url, tabId }) => {
        if (signal.aborted) return
        setTab(tabId, { status: 'inspecting' })
        const w = newAgent()
        workers.push(w)
        try {
          const { findingAdds } = await runTurnOn(w, workerPrompt(url), signal, (e) => onEv(e, tabId))
          setTab(tabId, { status: 'done', findings: findingAdds })
          set({ findings: get().findings + findingAdds })
        } catch {
          setTab(tabId, { status: 'error' })
        } finally {
          void w.reset()
        }
      })
      workers.splice(0)

      // Phase 3.5 (probe/redteam intensity) — active single-shot probes against
      // the deduped in-scope endpoint set. intruder_run / burst_send stay
      // manual at every level, by design.
      if (intensity !== 'readonly' && !signal.aborted) {
        pushLog('phase', t('scan.log.probeCollect', { mode: intensity }))
        set({ currentTool: 'redteam: 엔드포인트 수집' })
        const { finalText: epText } = await runTurnOn(
          transport,
          `finding_list 와 list_endpoints 로 지금까지 기록된 엔드포인트를 중복 없이 모은다(같은 path 는 하나, 쿼리 파라미터 제거). HTTP 메서드를 아는 경우 "METHOD URL" 형식으로, 모르면 "GET URL". 최대 30개. 설명 없이 JSON 문자열 배열 한 줄만 출력.`,
          signal,
          (e) => onEv(e)
        )
        const endpoints = parseEndpoints(epText, site, 30)
        // path_probe lives in the probe profile; redteam also runs the Scanner
        // step below, so switch when either needs it.
        const needProbe = intensity === 'redteam' || endpoints.length > 0
        if (needProbe) await useToolProfileStore.getState().setProfile('probe')

        // Scanner (Decepticon-style content discovery) — redteam only. The STORE
        // runs it deterministically (not left to the agent's discretion) so the
        // hidden-path sweep always happens: path_probe walks a built-in wordlist
        // (.env, admin/, .git/HEAD, *.js.map, server-status, …) on the origin.
        if (intensity === 'redteam' && !signal.aborted) {
          pushLog('phase', t('scan.log.hiddenPaths'))
          const w = newAgent()
          workers.push(w)
          try {
            const { findingAdds } = await runTurnOn(
              w,
              `path_probe({ baseUrl: "${origin}" }) 를 한 번 호출해 숨은 경로를 스윕한다(기본 워드리스트: .env/.env.production, admin/·administrator/·wp-admin/, .git/HEAD·.git/config, *.js.map 소스맵, server-status, phpinfo.php, .well-known/security.txt 등). 200 또는 비기본(비정상) 응답을 돌려주는 경로만 finding_add 로 기록한다 — 민감 파일 노출/관리자 페이지/소스맵 누출을 심각도와 함께. 이미 finding_list 에 있으면 추가 금지. 범위(${site}) 밖은 건드리지 않는다. ${findingsLangLine()} 끝나면 발견 개수만 한 줄.`,
              signal,
              (e) => onEv(e)
            )
            if (findingAdds) set({ findings: get().findings + findingAdds })
          } catch {
            // path_probe may be unavailable or the turn errored — skip, don't
            // abort the whole sweep.
          } finally {
            void w.reset()
          }
          workers.splice(0)
        }

        if (endpoints.length) {
          // Intensity ladder: probe = unchanged replays + passive inspection;
          // redteam adds one mutated/payload request per endpoint.
          const levelLine =
            intensity === 'probe'
              ? '강도: probe — replay_request 는 캡처된 요청을 변경 없이 재생만 한다. payload_probe/path_probe/crlf_test 금지. security_inspect/scan_secrets/replay_request(변경 없음)만 사용.'
              : '강도: redteam — 각 체크마다 변형 요청 최대 1개까지 허용.'
          pushLog('phase', t('scan.log.probeRun', { n: endpoints.length }))
          await pool(endpoints, Math.min(endpoints.length, MAX_PARALLEL), async (ep) => {
            if (signal.aborted) return
            const w = newAgent()
            workers.push(w)
            try {
              // Category-driven checklist (OWASP) is active-only → redteam only.
              const m = /^(\S+)\s+(.+)$/.exec(ep)
              const checklist =
                intensity === 'redteam' ? checklistFor(m?.[1] ?? 'GET', m?.[2] ?? ep) : ''
              const { findingAdds } = await runTurnOn(
                w,
                `${REDTEAM_PROBE_PROMPT}\n\n${levelLine}\n대상: ${ep}\n사이트 범위: ${site}\n${checklist}\n${findingsLangLine()}`,
                signal,
                (e) => onEv(e)
              )
              if (findingAdds) set({ findings: get().findings + findingAdds })
            } finally {
              void w.reset()
            }
          })
          workers.splice(0)
        }
        if (needProbe) await useToolProfileStore.getState().setProfile('reverse')
      }

      // Phase 4 — the orchestrator exports the combined report. Reset first:
      // its session sat idle through the (minutes-long) probe phase and its MCP
      // (SSE) connection can drop, making finding_export uncallable. A fresh
      // session reconnects; findings live in the main process, so none are lost.
      pushLog('phase', t('scan.log.report'))
      await transport.reset()
      const { exportText, finalText } = await runTurnOn(
        transport,
        `지금까지 기록된 finding 으로 finding_export 를 호출하고, 반환된 Markdown 보고서를 그대로 출력한다.`,
        signal,
        (e) => onEv(e)
      )
      pushLog('phase', t('scan.log.done', { n: get().findings }))
      set({ report: exportText || finalText || '(보고서가 비어 있습니다)', phase: 'done' })
    } catch (e) {
      if (stopRequested && transport) {
        // Stopped mid-scan: export what's already in the findings store.
        try {
          await useToolProfileStore.getState().setProfile('reverse')
          await transport.reset()
          const { exportText, finalText } = await runTurnOn(
            transport,
            `finding_export 를 호출하고 반환된 Markdown 보고서를 그대로 출력한다.`,
            new AbortController().signal
          )
          const body = exportText || finalText || '(중지됨 — 기록된 발견 없음)'
          set({ report: `${body}\n\n_— 스캔 중지됨 (부분 결과)_`, phase: 'done' })
        } catch (e2) {
          set({ errorText: e2 instanceof Error ? e2.message : String(e2), phase: 'error' })
        }
      } else {
        const msg = e instanceof Error ? e.message : String(e)
        pushLog('error', msg)
        set({ errorText: msg, phase: 'error' })
      }
    } finally {
      setRedTeamScan(false)
      stopRequested = false
      for (const w of workers.splice(0)) void w.reset()
      abort = null
    }
  }
}))
