import { useEffect, useRef, useState } from 'react'
import { AnimatePresence, motion } from 'framer-motion'

interface Site {
  url: string
  name: string
  desc: string
}

const SITES: Site[] = [
  {
    url: 'https://bot.sannysoft.com',
    name: 'Sannysoft',
    desc: 'Baseline stealth (webdriver / plugins / UA)'
  },
  {
    url: 'https://abrahamjuliot.github.io/creepjs/',
    name: 'CreepJS',
    desc: 'Full-stack fingerprint (canvas / audio / font / WebGL)'
  },
  {
    url: 'https://browserleaks.com/',
    name: 'BrowserLeaks',
    desc: 'Individual leaks (Canvas / WebGL / WebRTC)'
  },
  {
    url: 'https://amiunique.org/fingerprint',
    name: 'amiunique',
    desc: 'Uniqueness score'
  },
  {
    url: 'https://pixelscan.net/',
    name: 'pixelscan',
    desc: 'Consistency cross-check (UA / OS / WebGL)'
  }
]

const SPRING = { type: 'spring' as const, stiffness: 320, damping: 32 }

interface Props {
  onNavigate: (url: string) => void
}

export function BotCheckButton({ onNavigate }: Props) {
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
            <div className="botcheck-menu-title">Bot detection probes</div>
            {SITES.map((s) => (
              <button
                key={s.url}
                type="button"
                className="botcheck-item"
                onClick={() => go(s.url)}
              >
                <span className="botcheck-item-name">{s.name}</span>
                <span className="botcheck-item-desc">{s.desc}</span>
              </button>
            ))}
          </motion.div>
        )}
      </AnimatePresence>

      <button
        type="button"
        className={`chip botcheck-trigger${open ? ' active' : ''}`}
        onClick={() => setOpen((v) => !v)}
        title="Open bot detection test sites"
      >
        Bot check
      </button>
    </div>
  )
}
