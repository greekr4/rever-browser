import { useEffect, useRef, useState } from 'react'
import { AnimatePresence, motion } from 'framer-motion'

import { useT } from '@/stores/i18n'
import type { TKey } from '@/locales/en'

interface Site {
  url: string
  name: string
  descKey: TKey
}

const SITES: Site[] = [
  { url: 'https://bot.sannysoft.com', name: 'Sannysoft', descKey: 'botcheck.sannysoft.desc' },
  {
    url: 'https://abrahamjuliot.github.io/creepjs/',
    name: 'CreepJS',
    descKey: 'botcheck.creepjs.desc'
  },
  {
    url: 'https://browserleaks.com/',
    name: 'BrowserLeaks',
    descKey: 'botcheck.browserleaks.desc'
  },
  { url: 'https://amiunique.org/fingerprint', name: 'amiunique', descKey: 'botcheck.amiunique.desc' },
  { url: 'https://pixelscan.net/', name: 'pixelscan', descKey: 'botcheck.pixelscan.desc' }
]

const SPRING = { type: 'spring' as const, stiffness: 320, damping: 32 }

interface Props {
  onNavigate: (url: string) => void
}

export function BotCheckButton({ onNavigate }: Props) {
  const t = useT()
  const [open, setOpen] = useState(false)
  const wrapRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false)
    }
    const onMouse = (e: MouseEvent) => {
      const t = e.target as Node | null
      if (t && wrapRef.current?.contains(t)) return
      setOpen(false)
    }
    document.addEventListener('keydown', onKey)
    document.addEventListener('mousedown', onMouse)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('mousedown', onMouse)
    }
  }, [open])

  const go = (url: string) => {
    onNavigate(url)
    setOpen(false)
  }

  return (
    <div className="botcheck" ref={wrapRef}>
      <AnimatePresence>
        {open && (
          <motion.div
            className="botcheck-menu"
            initial={{ opacity: 0, y: 8, scale: 0.96 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 8, scale: 0.96 }}
            transition={SPRING}
          >
            <div className="botcheck-menu-title">{t('botcheck.menuTitle')}</div>
            {SITES.map((s) => (
              <button
                key={s.url}
                type="button"
                className="botcheck-item"
                onClick={() => go(s.url)}
              >
                <span className="botcheck-item-name">{s.name}</span>
                <span className="botcheck-item-desc">{t(s.descKey)}</span>
              </button>
            ))}
          </motion.div>
        )}
      </AnimatePresence>

      <button
        type="button"
        className={`chip botcheck-trigger${open ? ' active' : ''}`}
        onClick={() => setOpen((v) => !v)}
        title={t('botcheck.triggerTitle')}
      >
        {t('botcheck.trigger')}
      </button>
    </div>
  )
}
