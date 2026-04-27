import { create } from 'zustand'
import { persist } from 'zustand/middleware'

import { ACP_PERMISSION_TIMEOUT_MS } from '@/constants'

import type { RequestPermissionRequest, RequestPermissionResponse } from '@agentclientprotocol/sdk'

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

export function requestPermissionFromUser(
  params: RequestPermissionRequest
): Promise<RequestPermissionResponse> {
  if (useAutoApproveStore.getState().autoApprove) {
    return Promise.resolve({
      outcome: { outcome: 'selected', optionId: findBestAllowOption(params) }
    })
  }

  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      removeEntry(entry)
      resolve({ outcome: { outcome: 'selected', optionId: findRejectOption(params) } })
    }, ACP_PERMISSION_TIMEOUT_MS)

    const entry: PendingPermission = { request: params, resolve, timer }
    usePermissionStore.getState().push(entry)
  })
}

export function respondToPermission(optionId: string) {
  const entry = usePermissionStore.getState().queue.at(0)
  if (!entry) return
  removeEntry(entry)
  entry.resolve({ outcome: { outcome: 'selected', optionId } })
}

export function rejectCurrentPermission() {
  const entry = usePermissionStore.getState().queue.at(0)
  if (!entry) return
  removeEntry(entry)
  entry.resolve({
    outcome: { outcome: 'selected', optionId: findRejectOption(entry.request) }
  })
}
