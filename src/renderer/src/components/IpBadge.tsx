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
        setError(res.ip ? null : res.error ?? t('ip.unknownError'))
      })
  }, [t])

  useEffect(() => {
    refresh()
  }, [refresh, activeId, refreshSignal])

  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 4,
        flexShrink: 0,
        marginLeft: 8,
        marginBottom: 6
      }}
    >
      <button
        className="toolbar-btn"
        type="button"
        onClick={refresh}
        title={
          hidden
            ? t('ip.hiddenTitle')
            : error
              ? t('ip.errorTitle', { error })
              : t('ip.title')
        }
        style={
          {
            fontFamily: 'ui-monospace, monospace',
            fontSize: 11,
            color: 'var(--text-dim)',
            WebkitAppRegion: 'no-drag'
          } as React.CSSProperties
        }
      >
        {hidden ? t('ip.masked') : loading ? '…' : (ip ?? t('ip.none'))}
      </button>
      <button
        className="toolbar-btn"
        type="button"
        onClick={toggleHidden}
        title={hidden ? t('ip.showTitle') : t('ip.hideTitle')}
        style={
          {
            fontSize: 11,
            color: 'var(--text-dim)',
            WebkitAppRegion: 'no-drag'
          } as React.CSSProperties
        }
      >
        {hidden ? t('ip.show') : t('ip.hide')}
      </button>
    </div>
  )
}
