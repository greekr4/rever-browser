import { useEffect, useRef, useState } from 'react'
import { AnimatePresence, motion } from 'framer-motion'

import { TrafficList } from '@/components/network/TrafficList'
import { ConsolePanel } from '@/components/panels/ConsolePanel'
import { CookiesPanel } from '@/components/panels/CookiesPanel'
import { ExceptionsPanel } from '@/components/panels/ExceptionsPanel'
import { HistoryPanel } from '@/components/panels/HistoryPanel'
import { WebSocketPanel } from '@/components/panels/WebSocketPanel'
import { RepeaterPanel } from '@/components/repeater/RepeaterPanel'
import { useResizable } from '@/hooks/use-resizable'
import { useHistoryStore } from '@/stores/history'
import { useRepeaterStore } from '@/stores/repeater'
import { useTrafficStore } from '@/stores/traffic'

type PanelId = 'traffic' | 'console' | 'exceptions' | 'websocket' | 'repeater' | 'storage' | 'history'

interface FloatingChipsProps {
  openPanel: PanelId | null
  setOpenPanel: (p: PanelId | null) => void
}

const BOTTOM_PANEL_CONFIG = {
  initial: 320,
  min: 180,
  max: 600,
  storageKey: 'rev:bottom-h',
  axis: 'y' as const,
  side: 'top' as const
}

export function FloatingChips({ openPanel, setOpenPanel }: FloatingChipsProps) {
  const trafficCount = useTrafficStore((s) => s.order.length)
  const repeaterSourceId = useRepeaterStore((s) => s.sourceRequestId)
  const repeaterHistoryLen = useRepeaterStore((s) => s.history.length)
  const historyCount = useHistoryStore((s) => s.entries.length)

  // Auto-open the Repeater panel whenever a request gets sent into it from
  // somewhere else in the app (TrafficList "Send to Repeater" button).
  useEffect(() => {
    if (repeaterSourceId) setOpenPanel('repeater')
  }, [repeaterSourceId, setOpenPanel])

  const [consoleCount, setConsoleCount] = useState(0)
  const [exceptionCount, setExceptionCount] = useState(0)
  const [wsCount, setWsCount] = useState(0)

  const bottomPanel = useResizable(BOTTOM_PANEL_CONFIG)

  const panelRef = useRef<HTMLDivElement>(null)
  const chipStackRef = useRef<HTMLDivElement>(null)

  // Poll counts for badges
  useEffect(() => {
    const poll = async () => {
      const [logs, exceptions, wsList] = await Promise.all([
        window.rev.console.list(0),
        window.rev.console.exceptions(),
        window.rev.ws.list()
      ])
      setConsoleCount(logs.length)
      setExceptionCount(exceptions.length)
      setWsCount(wsList.length)
    }
    void poll()
    const id = setInterval(() => void poll(), 2000)
    return () => clearInterval(id)
  }, [])

  // ESC to close
  useEffect(() => {
    if (!openPanel) return
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpenPanel(null)
    }
    document.addEventListener('keydown', handler)
    return () => document.removeEventListener('keydown', handler)
  }, [openPanel, setOpenPanel])

  // Outside click to close — but ignore clicks inside the detail drawer
  // and the agent panel (so users can interact with detail/agent without
  // collapsing the bottom panel).
  useEffect(() => {
    if (!openPanel) return
    const handler = (e: MouseEvent) => {
      const target = e.target as Element | null
      if (!target) return
      if (panelRef.current?.contains(target)) return
      if (chipStackRef.current?.contains(target)) return
      if (target.closest?.('.detail-drawer')) return
      if (target.closest?.('.agent-panel')) return
      setOpenPanel(null)
    }
    document.addEventListener('mousedown', handler)
    return () => document.removeEventListener('mousedown', handler)
  }, [openPanel, setOpenPanel])

  const toggle = (id: PanelId) => {
    setOpenPanel(openPanel === id ? null : id)
  }

  const panelTitle: Record<PanelId, string> = {
    traffic: 'Traffic',
    console: 'Console',
    exceptions: 'Exceptions',
    websocket: 'WebSocket',
    repeater: 'Repeater',
    storage: 'Storage',
    history: 'History'
  }

  // The bottom menu bar is fixed at the bottom; the panel slides up above it.
  const SPRING = { type: 'spring' as const, stiffness: 320, damping: 32 }

  return (
    <>
      {/* Bottom menu bar — full width, fixed at bottom of webview area */}
      <div className="chip-stack" ref={chipStackRef}>
        <ChipButton
          label="Traffic"
          active={openPanel === 'traffic'}
          badge={trafficCount > 0 ? String(trafficCount) : undefined}
          onClick={() => toggle('traffic')}
        />
        <ChipButton
          label="Console"
          active={openPanel === 'console'}
          badge={consoleCount > 0 ? String(consoleCount) : undefined}
          onClick={() => toggle('console')}
        />
        <ChipButton
          label="Exceptions"
          active={openPanel === 'exceptions'}
          badge={exceptionCount > 0 ? String(exceptionCount) : undefined}
          onClick={() => toggle('exceptions')}
        />
        <ChipButton
          label="WebSocket"
          active={openPanel === 'websocket'}
          badge={wsCount > 0 ? String(wsCount) : undefined}
          onClick={() => toggle('websocket')}
        />
        <ChipButton
          label="Repeater"
          active={openPanel === 'repeater'}
          badge={
            repeaterSourceId
              ? repeaterHistoryLen > 0
                ? String(repeaterHistoryLen)
                : '•'
              : undefined
          }
          onClick={() => toggle('repeater')}
        />
        <ChipButton
          label="Storage"
          active={openPanel === 'storage'}
          onClick={() => toggle('storage')}
        />
        <ChipButton
          label="History"
          active={openPanel === 'history'}
          badge={historyCount > 0 ? String(historyCount) : undefined}
          onClick={() => toggle('history')}
        />
      </div>

      {/* Bottom slide panel */}
      <AnimatePresence>
        {openPanel && (
          <motion.div
            key={openPanel}
            className="chip-panel-bottom"
            ref={panelRef}
            style={{ height: bottomPanel.width }}
            initial={{ y: '100%' }}
            animate={{ y: 0 }}
            exit={{ y: '100%' }}
            transition={SPRING}
          >
            {/* Top-edge resize splitter */}
            <div
              className="chip-panel-splitter"
              onMouseDown={bottomPanel.startDrag}
              title="Resize"
            />

            {/* Panel header */}
            <div className="chip-panel-header">
              <span className="chip-panel-title">{panelTitle[openPanel]}</span>
              <button
                className="chip-panel-close"
                onClick={() => {
                  setOpenPanel(null)
                }}
                title="Close (Esc)"
              >
                ✕
              </button>
            </div>

            {/* Panel content */}
            <div className="chip-panel-content">
              {openPanel === 'traffic' && <TrafficList />}
              {openPanel === 'console' && <ConsolePanel />}
              {openPanel === 'exceptions' && <ExceptionsPanel />}
              {openPanel === 'websocket' && <WebSocketPanel />}
              {openPanel === 'repeater' && <RepeaterPanel />}
              {openPanel === 'storage' && <CookiesPanel />}
              {openPanel === 'history' && <HistoryPanel />}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </>
  )
}

interface ChipButtonProps {
  label: string
  active: boolean
  badge?: string
  onClick: () => void
}

function ChipButton({ label, active, badge, onClick }: ChipButtonProps) {
  return (
    <button
      className={`chip${active ? ' active' : ''}`}
      onClick={onClick}
    >
      <span>{label}</span>
      {badge && (
        <span className="chip-count" style={{ marginLeft: 4 }}>
          {badge}
        </span>
      )}
    </button>
  )
}
