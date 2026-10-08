import { create } from 'zustand'
import { persist } from 'zustand/middleware'

import { ACP_PERMISSION_TIMEOUT_MS } from '@/constants'
import { recordApprovalAudit } from '@/stores/approval-audit'

import {
  chooseAllowOption,
  extractTargetHost,
  findRejectOption,
  isRiskyToolRequest,
  matchedRiskyTool,
  sanitizeSelection
} from './risky-tools'
import { grantSession, hasSessionGrant } from './session-grants'

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
    const optionId = chooseAllowOption(params)
    audit(params, optionId, { auto: true, risky: false })
    return Promise.resolve({ outcome: { outcome: 'selected', optionId } })
  }

  // The user already chose "allow all this session" for this gated tool.
  // allow_once still — the grant lives here, so every call is audited.
  if (hasSessionGrant(params)) {
    const optionId = chooseAllowOption(params)
    audit(params, optionId, { auto: true, risky })
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

export interface ApproveOptions {
  /** Also allow every later call of this gated tool in the same session. */
  grantSession?: boolean
}

function decide(entry: PendingPermission, optionId: string) {
  removeEntry(entry)
  audit(entry.request, optionId, { auto: false, risky: isRiskyToolRequest(entry.request) })
  entry.resolve({ outcome: { outcome: 'selected', optionId } })
}

// After a session grant, calls of that tool already waiting in the queue are
// covered by it too — resolve them instead of prompting one by one.
function applySessionGrant(request: RequestPermissionRequest) {
  grantSession(request)
  for (const queued of [...usePermissionStore.getState().queue]) {
    if (!hasSessionGrant(queued.request)) continue
    const optionId = chooseAllowOption(queued.request)
    removeEntry(queued)
    audit(queued.request, optionId, { auto: true, risky: isRiskyToolRequest(queued.request) })
    queued.resolve({ outcome: { outcome: 'selected', optionId } })
  }
}

export function respondToPermission(picked: string, opts: ApproveOptions = {}) {
  const entry = usePermissionStore.getState().queue.at(0)
  if (!entry) return
  const optionId = sanitizeSelection(entry.request, picked)
  decide(entry, optionId)
  if (opts.grantSession && decisionFor(entry.request, optionId) === 'allow') {
    applySessionGrant(entry.request)
  }
}

export function approveCurrentPermission(opts: ApproveOptions = {}) {
  const entry = usePermissionStore.getState().queue.at(0)
  if (!entry) return
  decide(entry, chooseAllowOption(entry.request))
  if (opts.grantSession) applySessionGrant(entry.request)
}

export function rejectCurrentPermission() {
  const entry = usePermissionStore.getState().queue.at(0)
  if (!entry) return
  decide(entry, findRejectOption(entry.request))
}

export function hasPendingPermission(sessionId: string): boolean {
  return usePermissionStore.getState().queue.some((e) => e.request.sessionId === sessionId)
}

// Stop: ACP requires the client to answer every pending permission request of
// a cancelled turn with the `cancelled` outcome. Without this the stale prompt
// stays queued and pops up again in front of the next turn's prompt.
export function cancelPendingPermissions(sessionId: string) {
  for (const entry of [...usePermissionStore.getState().queue]) {
    if (entry.request.sessionId !== sessionId) continue
    removeEntry(entry)
    audit(entry.request, findRejectOption(entry.request), {
      auto: true,
      risky: isRiskyToolRequest(entry.request)
    })
    entry.resolve({ outcome: { outcome: 'cancelled' } })
  }
}
