import { useEffect, useState } from 'react'

interface Options {
  initial: number
  min: number
  max: number
  /** localStorage key to persist size */
  storageKey?: string
  /** drag direction relative to handle (default 'left' = handle on left edge, dragging left increases width) */
  side?: 'left' | 'right' | 'top' | 'bottom'
  /** axis of resize (default 'x') */
  axis?: 'x' | 'y'
}

export function useResizable({ initial, min, max, storageKey, side = 'left', axis = 'x' }: Options) {
  const [width, setWidth] = useState<number>(() => {
    if (storageKey) {
      const saved = localStorage.getItem(storageKey)
      if (saved) {
        const n = parseInt(saved, 10)
        if (Number.isFinite(n)) return Math.max(min, Math.min(max, n))
      }
    }
    return initial
  })

  useEffect(() => {
    if (storageKey) localStorage.setItem(storageKey, String(width))
  }, [storageKey, width])

  const startDrag = (e: React.MouseEvent) => {
    e.preventDefault()
    const startPos = axis === 'x' ? e.clientX : e.clientY
    const startW = width
    const cursor = axis === 'x' ? 'col-resize' : 'ns-resize'
    const onMove = (ev: MouseEvent) => {
      const pos = axis === 'x' ? ev.clientX : ev.clientY
      const delta = (side === 'left' || side === 'top') ? startPos - pos : pos - startPos
      const next = Math.max(min, Math.min(max, startW + delta))
      setWidth(next)
    }
    const onUp = () => {
      document.removeEventListener('mousemove', onMove)
      document.removeEventListener('mouseup', onUp)
      document.body.style.cursor = ''
      document.body.style.userSelect = ''
      document.body.classList.remove('resizing')
    }
    document.addEventListener('mousemove', onMove)
    document.addEventListener('mouseup', onUp)
    document.body.style.cursor = cursor
    document.body.style.userSelect = 'none'
    document.body.classList.add('resizing')
  }

  return { width, startDrag, setWidth }
}
