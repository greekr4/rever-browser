import { useEffect } from 'react'
import { AnimatePresence, motion } from 'framer-motion'

import { TrafficDetailDrawer } from '@/components/network/TrafficDetailDrawer'
import { useResizable } from '@/hooks/use-resizable'
import { useTrafficStore } from '@/stores/traffic'

export function DetailDrawer() {
  const detailId = useTrafficStore((s) => s.detailId)
  const closeDetail = useTrafficStore((s) => s.closeDetail)
  const detail = useResizable({ initial: 480, min: 320, max: 720, storageKey: 'rev:detail-w' })

  useEffect(() => {
    if (!detailId) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeDetail()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [detailId, closeDetail])

  return (
    <AnimatePresence>
      {detailId && (
        <motion.div
          className="detail-drawer"
          style={{ width: detail.width }}
          initial={{ x: '100%' }}
          animate={{ x: 0 }}
          exit={{ x: '100%' }}
          transition={{ type: 'spring', stiffness: 320, damping: 32 }}
        >
          <div className="detail-drawer-splitter" onMouseDown={detail.startDrag} />
          <TrafficDetailDrawer requestId={detailId} onClose={closeDetail} />
        </motion.div>
      )}
    </AnimatePresence>
  )
}
