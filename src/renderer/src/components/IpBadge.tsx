import { useCallback, useEffect, useRef, useState } from 'react'

import { useT } from '@/stores/i18n'
import { useIpVisibilityStore } from '@/stores/ip-visibility'
import { useTabsStore } from '@/stores/tabs'

interface Props {
  // Bumped by the parent after a proxy change is applied so the badge
  // re-checks the egress IP through the freshly configured session.
  refreshSignal: number
}

// Always-on egress IP badge for the titlebar. Shows the public IP as seen
// through the ACTIVE tab's session, so a tab proxy (isolated partition) is
// reflected. Refreshes on mount, on active-tab change, when refreshSignal
// bumps, and on click. A separate toggle masks the display (e.g. before
// screen recording) without pausing the underlying refresh.
export function IpBadge({ refreshSignal }: Props): React.ReactElement {
  const t = useT()
  const activeId = useTabsStore((s) => s.activeId)
  const hidden = useIpVisibilityStore((s) => s.hidden)
  const toggleHidden = useIpVisibilityStore((s) => s.toggle)
  const [ip, setIp] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  // Monotonic request id — a late response from a superseded fetch is dropped.
  const seq = useRef(0)

  // No deps: useT()'s `t` is a fresh function every render, so depending on it
  // would recreate `refresh`, re-run the effect each render and keep `loading`
  // stuck true (the IP shows "…" forever even though it resolved). The error
  // string is localized at render time instead.
  const refresh = useCallback(() => {
    const id = ++seq.current
    setLoading(true)
    void window.rev
      .getCurrentIp(useTabsStore.getState().activeId)
      .catch((e: unknown) => ({ error: e instanceof Error ? e.message : String(e) }))
      .then((res: { ip?: string; error?: string }) => {
        if (id !== seq.current) return
        setLoading(false)
        setIp(res.ip ?? null)
        setError(res.ip ? null : res.error ?? 'unknown')
      })
  }, [])

  useEffect(() => {
    refresh()
  }, [refresh, activeId, refreshSignal])

  // One pill: shows the egress IP (or masked), click toggles visibility.
  // Still auto-refreshes on mount / active-tab change / proxy change; a manual
  // re-check is a right-click.
  return (
    <button
      className="toolbar-btn"
      type="button"
      onClick={toggleHidden}
      onContextMenu={(e) => {
        e.preventDefault()
        refresh()
      }}
      title={
        hidden ? t('ip.showTitle') : error ? t('ip.errorTitle', { error }) : t('ip.title')
      }
      style={
        {
          display: 'flex',
          alignItems: 'center',
          gap: 5,
          flexShrink: 0,
          marginLeft: 8,
          marginBottom: 2,
          fontFamily: 'ui-monospace, monospace',
          fontSize: 11,
          color: 'var(--text-dim)',
          WebkitAppRegion: 'no-drag'
        } as React.CSSProperties
      }
    >
      <span>IP:</span>
      <span
        style={{
          filter: hidden ? 'blur(5px)' : undefined,
          userSelect: hidden ? 'none' : undefined,
          transition: 'filter 0.1s'
        }}
      >
        {loading ? '…' : (ip ?? '—')}
      </span>
    </button>
  )
}
