import { useEffect, useRef, useState, type ReactNode } from 'react'
import { AnimatePresence, motion } from 'framer-motion'

import { TrafficList } from '@/components/network/TrafficList'
import { ApiMapPanel } from '@/components/panels/ApiMapPanel'
import { useScanStore } from '@/stores/scan'
import { useT } from '@/stores/i18n'
import type { TKey } from '@/locales/en'
import { useTabsStore } from '@/stores/tabs'
import { ConsolePanel } from '@/components/panels/ConsolePanel'
import { CookiesPanel } from '@/components/panels/CookiesPanel'
import { ExceptionsPanel } from '@/components/panels/ExceptionsPanel'
import { HistoryPanel } from '@/components/panels/HistoryPanel'
import { WebSocketPanel } from '@/components/panels/WebSocketPanel'
import { RepeaterPanel } from '@/components/repeater/RepeaterPanel'
import { WsRepeaterPanel } from '@/components/repeater/WsRepeaterPanel'
import { useResizable } from '@/hooks/use-resizable'
import { useHistoryStore } from '@/stores/history'
import { useRepeaterStore } from '@/stores/repeater'
import { useTrafficStore } from '@/stores/traffic'
import { WorkflowPanel } from '@/workflows'
import { useWorkflowsStore } from '@/workflows/core/store'

type PanelId = 'traffic' | 'apimap' | 'console' | 'exceptions' | 'websocket' | 'repeater' | 'wsrepeater' | 'storage' | 'history' | 'workflows'

const svg = (...d: string[]): ReactNode => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.9} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    {d.map((p) => (p.startsWith('c:') ? <circle key={p} {...circle(p)} /> : <path key={p} d={p} />))}
  </svg>
)
// 'c:cx,cy,r' draws a circle; everything else is a path.
const circle = (p: string): { cx: number; cy: number; r: number } => {
  const [cx, cy, r] = p.slice(2).split(',').map(Number)
  return { cx, cy, r }
}

const ICON: Record<PanelId | 'redteam', ReactNode> = {
  traffic: svg('M7 4v16', 'M3 8l4-4 4 4', 'M17 20V4', 'M13 16l4 4 4-4'),
  apimap: svg('M3 6l6-3 6 3 6-3v15l-6 3-6-3-6 3z', 'M9 3v15', 'M15 6v15'),
  websocket: svg('M13 2L4 14h8l-1 8 9-12h-8z'),
  console: svg('M5 17l5-5-5-5', 'M12 19h7'),
  exceptions: svg('M10.3 3.9L1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z', 'M12 9v4', 'M12 17h.01'),
  repeater: svg('M17 2l4 4-4 4', 'M3 11V9a3 3 0 0 1 3-3h15', 'M7 22l-4-4 4-4', 'M21 13v2a3 3 0 0 1-3 3H3'),
  // The repeater's return arrow under a WebSocket bolt.
  wsrepeater: svg('M13 2L6 11h6l-1 5 7-9h-6z', 'M7 23l-4-4 4-4', 'M21 14v2a3 3 0 0 1-3 3H3'),
  storage: svg('M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3z', 'M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6', 'M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3'),
  history: svg('c:12,12,9', 'M12 7v5l3 2'),
  workflows: svg('c:6,6,2.5', 'c:6,18,2.5', 'c:18,12,2.5', 'M6 8.5v7', 'M8.5 6h3a4 4 0 0 1 4 4v-.5'),
  redteam: svg('c:12,12,9', 'c:12,12,5', 'c:12,12,1')
}

// Related panels sit together, split by thin dividers.
const GROUPS: PanelId[][] = [
  ['traffic', 'apimap', 'websocket'],
  ['console', 'exceptions'],
  ['repeater', 'wsrepeater', 'storage'],
  ['history', 'workflows']
]

const fmtCount = (n: number): string => n.toLocaleString('en-US')

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
  const apiCount = useTrafficStore((s) => Object.keys(s.endpoints).length)
  const repeaterSourceId = useRepeaterStore((s) => s.sourceRequestId)
  const repeaterLoadNonce = useRepeaterStore((s) => s.loadNonce)
  const repeaterHistoryLen = useRepeaterStore((s) => s.history.length)
  const historyCount = useHistoryStore((s) => s.entries.length)
  const workflowCount = useWorkflowsStore((s) => s.workflows.length)

  // Auto-open the Repeater panel whenever a request gets sent into it from
  // somewhere else in the app (TrafficList "↻R" button). Keyed on loadNonce —
  // which bumps on EVERY send — so re-sending the SAME request still switches
  // to the panel (sourceRequestId alone would be unchanged and do nothing).
  useEffect(() => {
    if (repeaterLoadNonce > 0) setOpenPanel('repeater')
  }, [repeaterLoadNonce, setOpenPanel])

  const [consoleCount, setConsoleCount] = useState(0)
  const [exceptionCount, setExceptionCount] = useState(0)
  const [wsCount, setWsCount] = useState(0)

  const bottomPanel = useResizable(BOTTOM_PANEL_CONFIG)

  const panelRef = useRef<HTMLDivElement>(null)
  const chipStackRef = useRef<HTMLDivElement>(null)

  // Poll counts for badges — three integers, not the full payloads.
  useEffect(() => {
    const poll = async () => {
      const c = await window.rev.counts()
      setConsoleCount(c.logs)
      setExceptionCount(c.exceptions)
      setWsCount(c.ws)
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
      // Resolve the panel from the live DOM, not panelRef: AnimatePresence
      // keys the panel on openPanel, so a traffic→repeater switch mounts a new
      // node and later nulls the shared ref when the old one finishes its exit
      // animation — leaving panelRef stale. querySelector always finds the
      // current panel.
      if (target.closest?.('.chip-panel-bottom')) return
      if (chipStackRef.current?.contains(target)) return
      if (target.closest?.('.detail-drawer')) return
      if (target.closest?.('.agent-panel')) return
      // Geometric fallback: the <webview> native layer can steal the mousedown
      // target on "empty" spots the panel visually covers, so also treat a
      // click within any live panel's (or the chip bar's) rect as inside.
      const panels = document.querySelectorAll<HTMLElement>('.chip-panel-bottom')
      for (const p of panels) if (pointInRect(e, p)) return
      if (pointInRect(e, chipStackRef.current)) return
      setOpenPanel(null)
    }
    document.addEventListener('mousedown', handler)
    return () => document.removeEventListener('mousedown', handler)
  }, [openPanel, setOpenPanel])

  const toggle = (id: PanelId) => {
    setOpenPanel(openPanel === id ? null : id)
  }

  const scanVisible = useScanStore((st) => st.visible)
  const openScan = useScanStore((st) => st.open)
  const activeTabUrl = useTabsStore((st) => st.tabs.find((t) => t.id === st.activeId)?.url ?? '')

  const tr = useT()
  const panelTitle = (id: PanelId): string => tr(`panel.${id}` as TKey)

  // Same counts the chip badges show, surfaced in the panel header too.
  const panelCount: Partial<Record<PanelId, number>> = {
    traffic: trafficCount,
    apimap: apiCount,
    console: consoleCount,
    exceptions: exceptionCount,
    websocket: wsCount,
    repeater: repeaterHistoryLen,
    history: historyCount,
    workflows: workflowCount
  }

  // The bottom menu bar is fixed at the bottom; the panel slides up above it.
  const SPRING = { type: 'spring' as const, stiffness: 320, damping: 32 }

  return (
    <>
      {/* Bottom menu bar — full width, fixed at bottom of webview area */}
      <div className="chip-stack" ref={chipStackRef} role="toolbar" aria-label={tr('panel.bar')}>
        {GROUPS.map((group, gi) => (
          <div key={gi} className="chip-group">
            {gi > 0 && <span className="chip-sep" aria-hidden="true" />}
            {group.map((id) => {
              const n = panelCount[id] ?? 0
              const badge =
                id === 'repeater'
                  ? repeaterSourceId
                    ? n > 0
                      ? fmtCount(n)
                      : '•'
                    : undefined
                  : id === 'storage' || n === 0
                    ? undefined
                    : fmtCount(n)
              return (
                <ChipButton
                  key={id}
                  icon={ICON[id]}
                  label={panelTitle(id)}
                  testId={`chip-${id}`}
                  active={openPanel === id}
                  badge={badge}
                  danger={id === 'exceptions' && n > 0}
                  onClick={() => toggle(id)}
                />
              )
            })}
          </div>
        ))}
        <span style={{ flex: 1 }} />
        {/* Red Team starts a scan rather than opening a panel, so it stands apart. */}
        <ChipButton
          icon={ICON.redteam}
          label={tr('panel.redteam')}
          testId="chip-redteam"
          action
          active={scanVisible}
          onClick={() => openScan(activeTabUrl)}
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
              <span className="chip-panel-title">
                {panelTitle(openPanel)}
                {(panelCount[openPanel] ?? 0) > 0 && (
                  <span className="chip-panel-count">{panelCount[openPanel]}</span>
                )}
              </span>
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
              {openPanel === 'apimap' && <ApiMapPanel />}
              {openPanel === 'console' && <ConsolePanel />}
              {openPanel === 'exceptions' && <ExceptionsPanel />}
              {openPanel === 'websocket' && <WebSocketPanel />}
              {openPanel === 'repeater' && <RepeaterPanel />}
              {openPanel === 'wsrepeater' && <WsRepeaterPanel />}
              {openPanel === 'storage' && <CookiesPanel />}
              {openPanel === 'history' && <HistoryPanel />}
              {openPanel === 'workflows' && <WorkflowPanel />}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </>
  )
}

/** True when the event's pointer coordinates fall within the element's rect. */
function pointInRect(e: MouseEvent, el: HTMLElement | null): boolean {
  if (!el) return false
  const r = el.getBoundingClientRect()
  return e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom
}

interface ChipButtonProps {
  label: string
  icon: ReactNode
  active: boolean
  badge?: string
  /** Red count badge (e.g. uncaught exceptions). */
  danger?: boolean
  /** Styled as a command (Red Team) rather than a panel tab. */
  action?: boolean
  testId?: string
  onClick: () => void
}

function ChipButton({ label, icon, active, badge, danger, action, testId, onClick }: ChipButtonProps) {
  return (
    <button
      type="button"
      className={`chip${active ? ' active' : ''}${action ? ' chip-action' : ''}`}
      data-testid={testId}
      aria-pressed={active}
      onClick={onClick}
    >
      {icon}
      <span>{label}</span>
      {badge && <span className={`chip-count${danger ? ' danger' : ''}`}>{badge}</span>}
    </button>
  )
}
