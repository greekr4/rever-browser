import { create } from 'zustand'
import { persist } from 'zustand/middleware'

// Whether the titlebar IpBadge shows the real egress IP or a masked
// placeholder. Persisted so it stays hidden across restarts once turned off
// for recording — the IP still refreshes in the background, only the render
// is masked.
interface IpVisibilityState {
  hidden: boolean
  toggle: () => void
}

export const useIpVisibilityStore = create<IpVisibilityState>()(
  persist(
    (set, get) => ({
      hidden: false,
      toggle: () => set({ hidden: !get().hidden })
    }),
    { name: 'rev:ip-hidden' }
  )
)
