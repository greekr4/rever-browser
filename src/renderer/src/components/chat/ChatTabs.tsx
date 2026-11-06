import { useCallback, useState } from 'react'

import type { ACPAgentID } from '@/constants'

import { useT } from '@/stores/i18n'
import { ChatPanel, type ChatTabMeta } from './ChatPanel'
import { ModelPicker } from './ModelPicker'
import { ProviderIcon } from './ProviderIcon'

let tabSeq = 0
const newTabId = (): string => `chat-${++tabSeq}`

/**
 * Orca-style session tabs: each tab is its own ChatPanel with its own agent
 * process. Inactive tabs stay mounted (hidden) so a running turn keeps going;
 * closing a tab unmounts its panel, which tears down that agent session.
 */
export function ChatTabs() {
  const tr = useT()
  const [firstId] = useState(newTabId)
  const [tabs, setTabs] = useState<string[]>([firstId])
  // The agent + model each later tab was opened with (the first tab restores its own).
  const [launch, setLaunch] = useState<Record<string, { agentId: ACPAgentID }>>({})
  const [activeId, setActiveId] = useState(firstId)
  const [meta, setMeta] = useState<Record<string, ChatTabMeta>>({})

  const onMeta = useCallback((id: string, m: ChatTabMeta) => {
    setMeta((prev) => {
      const old = prev[id]
      if (old && old.title === m.title && old.busy === m.busy && old.agentId === m.agentId) return prev
      return { ...prev, [id]: m }
    })
  }, [])

  const add = (agentId: ACPAgentID): void => {
    const id = newTabId()
    setLaunch((l) => ({ ...l, [id]: { agentId } }))
    setTabs((t) => [...t, id])
    setActiveId(id)
  }

  const close = (id: string): void => {
    const rest = tabs.filter((t) => t !== id)
    if (rest.length === 0) {
      // Always keep one session open.
      const fresh = newTabId()
      setTabs([fresh])
      setActiveId(fresh)
    } else {
      setTabs(rest)
      if (id === activeId) setActiveId(rest[Math.max(0, tabs.indexOf(id) - 1)])
    }
    setMeta(({ [id]: _gone, ...keep }) => keep)
    setLaunch(({ [id]: _gone, ...keep }) => keep)
  }

  return (
    <div style={{ flex: 1, minHeight: 0, display: 'flex', flexDirection: 'column' }}>
      <div className="chat-tabs" role="tablist" aria-label={tr('chat.tabs')}>
        {tabs.map((id) => {
          const m = meta[id]
          return (
            <div
              key={id}
              role="tab"
              tabIndex={0}
              aria-selected={id === activeId}
              className="chat-tab"
              data-testid="chat-tab"
              title={m?.title}
              onClick={() => setActiveId(id)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') setActiveId(id)
              }}
              onAuxClick={(e) => {
                if (e.button === 1) close(id)
              }}
            >
              {m && <ProviderIcon agentId={m.agentId} size={12} />}
              <span className="chat-tab-title">{m?.title ?? tr('chat.newTab')}</span>
              {m?.busy && <span className="chat-tab-busy" aria-label={tr('chat.tabBusy')} />}
              <button
                type="button"
                className="chrome-tab-close"
                aria-label={tr('chat.closeTab')}
                title={tr('chat.closeTab')}
                onClick={(e) => {
                  e.stopPropagation()
                  close(id)
                }}
              >
                ×
              </button>
            </div>
          )
        })}
        <ModelPicker
          newTab
          agentId={launch[activeId]?.agentId ?? 'claude-code'}
          modelName={null}
          onSelect={() => {}}
          onPickAgent={add}
        />
      </div>
      {tabs.map((id) => (
        <div
          key={id}
          role="tabpanel"
          style={{ flex: 1, minHeight: 0, display: id === activeId ? 'flex' : 'none', flexDirection: 'column' }}
        >
          <ChatPanel
            tabId={id}
            active={id === activeId}
            restore={id === firstId}
            onMeta={onMeta}
            initialAgent={launch[id]?.agentId}
          />
        </div>
      ))}
    </div>
  )
}
