import { create } from 'zustand'
import { persist } from 'zustand/middleware'

import { ACP_PERMISSION_TIMEOUT_MS } from '@/constants'
import { recordApprovalAudit } from '@/stores/approval-audit'

import {
  extractTargetHost,
  isRiskyToolRequest,
  matchedRiskyTool
} from './risky-tools'

import type {
  PermissionOption,
  RequestPermissionRequest,
  RequestPermissionResponse
} from '@agentclientprotocol/sdk'

export interface PendingPermission {
  request: RequestPermissionRequest
  resolve: (response: RequestPermissionResponse) => void
  timer: ReturnType<typeof setTimeout>
}

interface PermissionState {
  queue: PendingPermission[]
  push: (entry: PendingPermission) => void
  remove: (entry: PendingPermission) => void
}

const usePermissionStore = create<PermissionState>((set) => ({
  queue: [],
  push: (entry) => set((s) => ({ queue: [...s.queue, entry] })),
  remove: (entry) => set((s) => ({ queue: s.queue.filter((e) => e !== entry) }))
}))

interface AutoApproveState {
  autoApprove: boolean
  setAutoApprove: (v: boolean) => void
}

const useAutoApproveStore = create<AutoApproveState>()(
  persist(
    (set) => ({
      autoApprove: true,
      setAutoApprove: (v) => set({ autoApprove: v })
    }),
    { name: 'rever-browser:acp-auto-approve' }
  )
)

export const useCurrentPermission = () =>
  usePermissionStore((s) => s.queue[0] ?? null)

export const usePermissionQueue = () => usePermissionStore((s) => s.queue)

export const useAcpAutoApprove = () => useAutoApproveStore((s) => s.autoApprove)
export const setAcpAutoApprove = (v: boolean) =>
  useAutoApproveStore.getState().setAutoApprove(v)

function findRejectOption(request: RequestPermissionRequest): string {
  const reject = request.options.find((o) => o.kind.startsWith('reject'))
  return reject?.optionId ?? request.options.at(0)?.optionId ?? ''
}

function findBestAllowOption(request: RequestPermissionRequest): string {
  const allowAlways = request.options.find((o) => o.kind === 'allow_always')
  if (allowAlways) return allowAlways.optionId
  const allow = request.options.find((o) => o.kind.startsWith('allow'))
  return allow?.optionId ?? request.options.at(0)?.optionId ?? ''
}

function removeEntry(entry: PendingPermission) {
  clearTimeout(entry.timer)
  usePermissionStore.getState().remove(entry)
}

function toolCallOf(
  request: RequestPermissionRequest
): { title?: string; rawInput?: unknown } | undefined {
  return (request as unknown as { toolCall?: { title?: string; rawInput?: unknown } }).toolCall
}

function auditLabel(request: RequestPermissionRequest): string {
  return matchedRiskyTool(request) ?? toolCallOf(request)?.title ?? 'tool'
}

function decisionFor(request: RequestPermissionRequest, optionId: string): 'allow' | 'reject' {
  const opt = request.options.find((o: PermissionOption) => o.optionId === optionId)
  return opt && opt.kind.startsWith('reject') ? 'reject' : 'allow'
}

function audit(
  request: RequestPermissionRequest,
  optionId: string,
  opts: { auto: boolean; risky: boolean }
) {
  recordApprovalAudit({
    tool: auditLabel(request),
    target: extractTargetHost(toolCallOf(request)?.rawInput),
    decision: decisionFor(request, optionId),
    auto: opts.auto,
    risky: opts.risky
  })
}

export function requestPermissionFromUser(
  params: RequestPermissionRequest
): Promise<RequestPermissionResponse> {
  const risky = isRiskyToolRequest(params)

  // Auto-approve never applies to risky tools: a side-effecting action on the
  // user's live session always surfaces the prompt, regardless of the setting.
  if (!risky && useAutoApproveStore.getState().autoApprove) {
    const optionId = findBestAllowOption(params)
    audit(params, optionId, { auto: true, risky: false })
    return Promise.resolve({ outcome: { outcome: 'selected', optionId } })
  }

  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      removeEntry(entry)
      const optionId = findRejectOption(params)
      audit(params, optionId, { auto: false, risky })
      resolve({ outcome: { outcome: 'selected', optionId } })
    }, ACP_PERMISSION_TIMEOUT_MS)

    const entry: PendingPermission = { request: params, resolve, timer }
    usePermissionStore.getState().push(entry)
  })
}

export function respondToPermission(optionId: string) {
  const entry = usePermissionStore.getState().queue.at(0)
  if (!entry) return
  removeEntry(entry)
  audit(entry.request, optionId, { auto: false, risky: isRiskyToolRequest(entry.request) })
  entry.resolve({ outcome: { outcome: 'selected', optionId } })
}

export function approveCurrentPermission() {
  const entry = usePermissionStore.getState().queue.at(0)
  if (!entry) return
  removeEntry(entry)
  const optionId = findBestAllowOption(entry.request)
  audit(entry.request, optionId, { auto: false, risky: isRiskyToolRequest(entry.request) })
  entry.resolve({ outcome: { outcome: 'selected', optionId } })
}

export function rejectCurrentPermission() {
  const entry = usePermissionStore.getState().queue.at(0)
  if (!entry) return
  removeEntry(entry)
  const optionId = findRejectOption(entry.request)
  audit(entry.request, optionId, { auto: false, risky: isRiskyToolRequest(entry.request) })
  entry.resolve({ outcome: { outcome: 'selected', optionId } })
}
