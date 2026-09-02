import { useT } from '@/stores/i18n'
import { useBookmarksStore } from '@/stores/bookmarks'
import { useNavigationRequestStore } from '@/stores/navigation-request'

// Same favicon source as HistoryPanel — Google's s2 service resolves the
// site's real favicon by hostname, so the bar looks like Chrome's without
// having to capture page-favicon-updated events per tab.
function Favicon({ url }: { url: string }): React.JSX.Element {
  let host = ''
  try {
    host = new URL(url).hostname
  } catch {
    /* ignore */
  }
  if (!host) {
    return (
      <span
        style={{
          width: 16,
          height: 16,
          background: 'var(--surface-3)',
          borderRadius: 2,
          flexShrink: 0
        }}
      />
    )
  }
  return (
    <img
      src={`https://www.google.com/s2/favicons?domain=${host}&sz=32`}
      width={16}
      height={16}
      alt=""
      style={{ flexShrink: 0, borderRadius: 2 }}
      onError={(e) => {
        e.currentTarget.style.visibility = 'hidden'
      }}
    />
  )
}

export function BookmarkBar(): React.JSX.Element | null {
  const t = useT()
  const bookmarks = useBookmarksStore((s) => s.bookmarks)
  const remove = useBookmarksStore((s) => s.remove)
  const requestNav = useNavigationRequestStore((s) => s.request)

  if (bookmarks.length === 0) return null

  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 2,
        padding: '3px 8px',
        borderBottom: '1px solid var(--border)',
        overflowX: 'auto',
        flexShrink: 0
      }}
    >
      {bookmarks.map((b) => (
        <button
          key={b.id}
          type="button"
          className="bookmark-chip"
          title={`${b.title}\n${b.url}`}
          onClick={() => requestNav(b.url)}
        >
          <Favicon url={b.url} />
          <span
            style={{
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
              maxWidth: 130
            }}
          >
            {b.title}
          </span>
          <span
            className="bookmark-chip-x"
            role="button"
            title={t('bookmark.remove')}
            onClick={(e) => {
              e.stopPropagation()
              remove(b.id)
            }}
          >
            ×
          </span>
        </button>
      ))}
    </div>
  )
}
