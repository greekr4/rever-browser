import { useTabsStore } from '@/stores/tabs'

export function TabBar() {
  const tabs = useTabsStore((s) => s.tabs)
  const activeId = useTabsStore((s) => s.activeId)
  const selectTab = useTabsStore((s) => s.selectTab)
  const closeTab = useTabsStore((s) => s.closeTab)
  const addTab = useTabsStore((s) => s.addTab)

  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'flex-end',
        gap: 2,
        padding: '4px 8px 0',
        borderBottom: '1px solid #2a2a2a',
        background: '#161616',
        overflowX: 'auto'
      }}
    >
      {tabs.map((t) => {
        const isActive = t.id === activeId
        return (
          <div
            key={t.id}
            onClick={() => selectTab(t.id)}
            onAuxClick={(e) => {
              if (e.button === 1) closeTab(t.id) // middle-click close
            }}
            style={{
              display: 'flex',
              alignItems: 'center',
              gap: 6,
              padding: '6px 8px 6px 10px',
              minWidth: 120,
              maxWidth: 220,
              cursor: 'pointer',
              borderRadius: '6px 6px 0 0',
              background: isActive ? '#0e0e0e' : 'transparent',
              borderTop: `1px solid ${isActive ? '#333' : 'transparent'}`,
              borderLeft: `1px solid ${isActive ? '#333' : 'transparent'}`,
              borderRight: `1px solid ${isActive ? '#333' : 'transparent'}`,
              fontSize: 12,
              color: isActive ? '#eee' : '#999',
              userSelect: 'none'
            }}
          >
            <span
              style={{
                flex: 1,
                overflow: 'hidden',
                textOverflow: 'ellipsis',
                whiteSpace: 'nowrap'
              }}
              title={t.url}
            >
              {t.title || t.url}
            </span>
            {tabs.length > 1 && (
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation()
                  closeTab(t.id)
                }}
                style={{
                  width: 16,
                  height: 16,
                  padding: 0,
                  display: 'flex',
                  alignItems: 'center',
                  justifyContent: 'center',
                  background: 'transparent',
                  border: 'none',
                  color: '#888',
                  cursor: 'pointer',
                  fontSize: 14,
                  lineHeight: 1
                }}
                title="Close tab"
              >
                ×
              </button>
            )}
          </div>
        )
      })}

      <button
        type="button"
        onClick={() => addTab('https://www.google.com')}
        style={{
          padding: '4px 10px',
          marginLeft: 4,
          marginBottom: 2,
          background: 'transparent',
          border: '1px solid #333',
          borderRadius: 4,
          color: '#bbb',
          cursor: 'pointer',
          fontSize: 14,
          lineHeight: 1
        }}
        title="New tab"
      >
        +
      </button>
    </div>
  )
}
