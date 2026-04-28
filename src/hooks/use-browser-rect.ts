import { invoke } from '@tauri-apps/api/core'
import { useEffect, useRef } from 'react'

export function useBrowserRect() {
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const el = ref.current
    if (!el) return

    let last: { x: number; y: number; w: number; h: number } | null = null

    const update = () => {
      const r = el.getBoundingClientRect()
      const next = { x: r.left, y: r.top, w: r.width, h: r.height }
      if (
        last &&
        last.x === next.x &&
        last.y === next.y &&
        last.w === next.w &&
        last.h === next.h
      )
        return
      last = next
      void invoke('browser_set_position', next).catch(() => null)
    }

    update()

    const observer = new ResizeObserver(update)
    observer.observe(el)
    window.addEventListener('resize', update)
    window.addEventListener('scroll', update, true)

    return () => {
      observer.disconnect()
      window.removeEventListener('resize', update)
      window.removeEventListener('scroll', update, true)
    }
  }, [])

  return ref
}
