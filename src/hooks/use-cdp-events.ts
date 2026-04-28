import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useEffect } from 'react'

import { useTrafficStore } from '@/stores/traffic'
import type { NetworkEvent } from '@/types/traffic'

export function useCdpEvents() {
  const applyEvent = useTrafficStore((s) => s.applyEvent)

  useEffect(() => {
    let unlisten: UnlistenFn | undefined
    let cancelled = false

    listen<NetworkEvent>('network-event', (e) => {
      applyEvent(e.payload)
    }).then((fn) => {
      if (cancelled) {
        fn()
      } else {
        unlisten = fn
      }
    })

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [applyEvent])
}
