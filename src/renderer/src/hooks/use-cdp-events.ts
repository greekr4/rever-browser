import { useEffect } from 'react'

import { useTrafficStore } from '@/stores/traffic'
import type { NetworkEvent } from '@/types/traffic'

export function useCdpEvents() {
  const applyEvent = useTrafficStore((s) => s.applyEvent)

  useEffect(() => {
    const unsubscribe = window.rev.onNetworkEvent((payload) => {
      applyEvent(payload as NetworkEvent)
    })
    return () => {
      unsubscribe()
    }
  }, [applyEvent])
}
