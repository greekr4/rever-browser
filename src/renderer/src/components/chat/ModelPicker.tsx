import { useEffect, useMemo, useRef, useState } from 'react'

import {
  ACP_AGENTS,
  type ACPAgentDef,
  type ACPAgentID,
  type CatalogModel
} from '@/constants'
import type { TKey } from '@/locales/en'
import { useAllAgentModels } from '@/stores/model-catalog'
import { useT } from '@/stores/i18n'

import type { AgentHealth, AgentHealthStatus } from '../../../../preload'
import { ProviderIcon } from './ProviderIcon'

// unified model picker: one searchable list of every model across
// providers, grouped by agent. Choosing a model implicitly selects its provider
// (agent). Providers that need setup show install/sign-in hints or an API-key
// input inline; only models whose provider is `ready` are selectable.

type KeyProvider = 'anthropic' | 'openai' | 'grok' | 'glm'

function isKeyProvider(def: ACPAgentDef): def is ACPAgentDef & { provider: KeyProvider } {
  return def.provider === 'anthropic' || def.provider === 'openai' || def.provider === 'grok' || def.provider === 'glm'
}

const API_SENTINEL = 'api'

const STATUS_COLOR: Record<AgentHealthStatus, string> = {
  ready: 'var(--status-ok)',
  'not-installed': 'var(--http-none)',
  'needs-key': 'var(--status-warn)',
  'needs-login': 'var(--status-warn)',
  'auth-failed': 'var(--status-error)',
  failed: 'var(--status-error)'
}

function resolvedPathFor(def: ACPAgentDef, h: AgentHealth | undefined): string {
  if (isKeyProvider(def)) return API_SENTINEL
  return h?.resolvedPath ?? def.command
}

interface ModelPickerProps {
  agentId: ACPAgentID
  /** Display name of the active model, or null to fall back to the agent name. */
  modelName: string | null
  onSelect: (agentId: ACPAgentID, resolvedPath: string, modelId: string, name: string) => void
  disabled?: boolean
  /** Render as the chat tabs' "+" button: lists agents only; the model is picked inside the session. */
  newTab?: boolean
  /** newTab mode: a ready agent was clicked. */
  onPickAgent?: (agentId: ACPAgentID) => void
}

export function ModelPicker({ agentId, modelName, onSelect, disabled, newTab, onPickAgent }: ModelPickerProps) {
  const tr = useT()
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [health, setHealth] = useState<Record<string, AgentHealth>>({})
  const [loading, setLoading] = useState(true)
  const [keyInput, setKeyInput] = useState<Record<string, string>>({})
  const [savingKey, setSavingKey] = useState<string | null>(null)
  const [copied, setCopied] = useState<string | null>(null)
  const popoverRef = useRef<HTMLDivElement>(null)
  const triggerRef = useRef<HTMLButtonElement>(null)
  // Fixed placement inside the chat column: an absolute popover got clipped by its overflow.
  const [fixedPos, setFixedPos] = useState<React.CSSProperties | null>(null)
  const openPicker = (): void => {
    if (!open && triggerRef.current) {
      const r = triggerRef.current.getBoundingClientRect()
      const col = triggerRef.current.closest('aside')?.getBoundingClientRect() ?? { left: 0, right: window.innerWidth }
      const width = Math.min(320, col.right - col.left - 16)
      const left = Math.max(col.left + 8, Math.min(r.left, col.right - width - 8))
      setFixedPos({ position: 'fixed', boxSizing: 'border-box', top: r.bottom + 6, left, width, maxHeight: window.innerHeight - r.bottom - 18 })
    }
    setOpen((v) => !v)
  }

  const scan = async () => {
    setLoading(true)
    const results = await window.rev.acp.probe()
    setHealth(Object.fromEntries(results.map((r) => [r.id, r])))
    setLoading(false)
  }

  useEffect(() => {
    let cancelled = false
    void window.rev.acp.probe().then((results) => {
      if (cancelled) return
      setHealth(Object.fromEntries(results.map((r) => [r.id, r])))
      setLoading(false)
    })
    return () => {
      cancelled = true
    }
  }, [])

  useEffect(() => {
    if (!open) return
    const onDocClick = (e: MouseEvent) => {
      const t = e.target as Node
      if (popoverRef.current?.contains(t) || triggerRef.current?.contains(t)) return
      setOpen(false)
    }
    document.addEventListener('mousedown', onDocClick)
    return () => document.removeEventListener('mousedown', onDocClick)
  }, [open])

  const selectedDef = ACP_AGENTS.find((a) => a.id === agentId) ?? ACP_AGENTS[0]
  const selectedStatus = health[selectedDef.id]?.status ?? 'failed'
  const triggerLabel = modelName ?? selectedDef.name

  const readyCount = useMemo(
    () => Object.values(health).filter((h) => h.status === 'ready').length,
    [health]
  )

  // Group the catalog by agent, preserving catalog order, and drop rows the
  // search query doesn't match (by model name or agent name).
  const catalog = useAllAgentModels()
  const groups = useMemo(() => {
    const q = query.trim().toLowerCase()
    const byAgent = new Map<ACPAgentID, CatalogModel[]>()
    for (const m of catalog) {
      const def = ACP_AGENTS.find((a) => a.id === m.agentId)
      if (!def) continue
      const hit =
        !q || m.name.toLowerCase().includes(q) || def.name.toLowerCase().includes(q)
      if (!hit) continue
      const list = byAgent.get(m.agentId) ?? []
      list.push(m)
      byAgent.set(m.agentId, list)
    }
    const ready = (id: string): number => (health[id]?.status === 'ready' ? 0 : 1)
    // Usable agents first; ones still needing a key or install sink to the bottom (stable within each).
    return ACP_AGENTS.filter((a) => byAgent.has(a.id))
      .map((a) => ({ def: a, models: byAgent.get(a.id) as CatalogModel[] }))
      .sort((x, y) => ready(x.def.id) - ready(y.def.id))
  }, [query, health, catalog])

  const choose = (def: ACPAgentDef, m: CatalogModel) => {
    if (health[def.id]?.status !== 'ready') return
    onSelect(def.id, resolvedPathFor(def, health[def.id]), m.modelId, m.name)
    setOpen(false)
  }

  const saveKey = async (provider: KeyProvider) => {
    const key = (keyInput[provider] ?? '').trim()
    if (!key) return
    setSavingKey(provider)
    try {
      await window.rev.settings.setApiKey(provider, key)
      setKeyInput((prev) => ({ ...prev, [provider]: '' }))
      await scan()
    } finally {
      setSavingKey(null)
    }
  }

  const copy = (text: string) => {
    void navigator.clipboard.writeText(text)
    setCopied(text)
    setTimeout(() => setCopied(null), 1500)
  }

  return (
    <div style={{ position: 'relative' }}>
      {newTab ? (
        <button
          ref={triggerRef}
          type="button"
          className="chrome-icon-btn small"
          onClick={openPicker}
          disabled={disabled}
          aria-expanded={open}
          title={tr('chat.newTab')}
          aria-label={tr('chat.newTab')}
          data-testid="chat-tab-new"
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" aria-hidden="true">
            <path d="M12 5v14M5 12h14" />
          </svg>
        </button>
      ) : (
        <button
          ref={triggerRef}
          type="button"
          onClick={openPicker}
          disabled={disabled}
          aria-expanded={open}
          style={triggerStyle}
          title={tr('chat.chooseAgent')}
        >
          <span style={iconChip}>
            <ProviderIcon agentId={selectedDef.id} size={14} fallback={selectedDef.icon} />
          </span>
          <span style={{ fontWeight: 500, maxWidth: 130, overflow: 'hidden', textOverflow: 'ellipsis' }}>
            {triggerLabel}
          </span>
          <span style={{ ...statusDot, background: STATUS_COLOR[selectedStatus] }} />
          <span style={{ opacity: 0.6, fontSize: 10 }}>▾</span>
        </button>
      )}

      {open && (
        <div
          ref={popoverRef}
          style={fixedPos ? { ...popoverStyle, ...fixedPos } : popoverStyle}
        >
          {!newTab && (
            <input
              autoFocus
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={tr('model.search')}
              style={searchStyle}
            />
          )}
          <div style={popoverHeader}>
            <span style={{ fontSize: 11, opacity: 0.6 }}>
              {loading ? tr('onboard.scanning') : tr('onboard.ready', { n: readyCount })}
            </span>
          </div>

          <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
            {groups.map(({ def, models }) => {
              const h = health[def.id]
              const status = h?.status ?? 'failed'
              const ready = status === 'ready'
              const statusLabel = h?.auto
                ? tr('onboard.status.auto')
                : tr(`onboard.status.${status}` as TKey)
              // The new-tab list only needs to say which agents can be opened.
              const statusText =
                newTab && ready
                  ? tr('chat.agentConnected')
                  : `${statusLabel}${h?.plan && h.plan !== 'api-key' ? ` · ${tr('onboard.plan', { plan: h.plan })}` : ''}`
              const hint =
                status === 'not-installed'
                  ? { title: tr('onboard.installWith'), cmd: def.installHint }
                  : status === 'needs-login' && def.loginHint
                    ? { title: tr('onboard.signInWith'), cmd: def.loginHint }
                    : null

              return (
                <div key={def.id}>
                  {newTab && ready ? (
                    <button
                      type="button"
                      data-testid={`new-tab-agent-${def.id}`}
                      onClick={() => {
                        onPickAgent?.(def.id)
                        setOpen(false)
                      }}
                      className="new-tab-agent"
                      style={{ ...modelRow, ...groupHeader, width: '100%', padding: '8px 8px', justifyContent: 'flex-start' }}
                    >
                    <span style={groupIcon}>
                      <ProviderIcon agentId={def.id} size={12} fallback={def.icon} />
                    </span>
                    <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: 0.3 }}>
                      {def.name}
                    </span>
                    <span style={{ fontSize: 10, color: STATUS_COLOR[status] }}>
                      {statusText}
                    </span>
                    </button>
                  ) : (
                    <div style={groupHeader}>
                    <span style={groupIcon}>
                      <ProviderIcon agentId={def.id} size={12} fallback={def.icon} />
                    </span>
                    <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: 0.3 }}>
                      {def.name}
                    </span>
                    <span style={{ fontSize: 10, color: STATUS_COLOR[status] }}>
                      {statusText}
                    </span>
                    </div>
                  )}

                  {ready ? (
                    !newTab && <div style={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
                      {models.map((m) => {
                        const active = !newTab && def.id === agentId && m.name === modelName
                        return (
                          <button
                            key={`${def.id}:${m.modelId}`}
                            type="button"
                            onClick={() => choose(def, m)}
                            style={active ? modelRowActive : modelRow}
                          >
                            <span>{m.name}</span>
                            {active && <span style={{ color: 'var(--accent-text)' }}>✓</span>}
                          </button>
                        )
                      })}
                    </div>
                  ) : (
                    <div style={{ display: 'flex', flexDirection: 'column', gap: 6, padding: '2px 4px' }}>
                      {hint && (
                        <div style={hintRow}>
                          <span style={{ fontSize: 10, color: 'var(--text-2)' }}>{hint.title}</span>
                          <code style={codeChip}>{hint.cmd}</code>
                          <button type="button" onClick={() => copy(hint.cmd)} style={linkBtn}>
                            {copied === hint.cmd ? tr('onboard.copied') : tr('onboard.copy')}
                          </button>
                        </div>
                      )}
                      {isKeyProvider(def) && (
                        <div style={hintRow}>
                          <input
                            type="password"
                            value={keyInput[def.provider] ?? ''}
                            onChange={(e) =>
                              setKeyInput((prev) => ({ ...prev, [def.provider]: e.target.value }))
                            }
                            onKeyDown={(e) => {
                              if (e.key === 'Enter') void saveKey(def.provider)
                            }}
                            placeholder={tr('onboard.keyPlaceholder')}
                            style={keyInputStyle}
                          />
                          <button
                            type="button"
                            onClick={() => void saveKey(def.provider)}
                            disabled={
                              savingKey === def.provider || !(keyInput[def.provider] ?? '').trim()
                            }
                            style={saveBtn}
                          >
                            {savingKey === def.provider ? '…' : tr('onboard.save')}
                          </button>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              )
            })}

            {groups.length === 0 && (
              <div style={{ fontSize: 11, color: 'var(--text-2)', padding: '8px 4px' }}>
                {tr('model.noMatch')}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  )
}

const triggerStyle: React.CSSProperties = {
  display: 'inline-flex',
  alignItems: 'center',
  gap: 6,
  padding: '4px 8px',
  background: 'var(--bg-bar)',
  border: '1px solid var(--border-2)',
  borderRadius: 6,
  color: 'var(--text)',
  cursor: 'pointer',
  fontSize: 12
}

const iconChip: React.CSSProperties = {
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  width: 18,
  height: 18,
  borderRadius: 4,
  background: 'var(--surface-3)',
  fontSize: 11,
  fontWeight: 700
}

const statusDot: React.CSSProperties = { width: 6, height: 6, borderRadius: '50%' }

const popoverStyle: React.CSSProperties = {
  position: 'absolute',
  top: 'calc(100% + 6px)',
  left: 0,
  zIndex: 50,
  width: 'min(320px, calc(var(--chat-w, 100vw) - 24px))',
  maxHeight: '70vh',
  overflowY: 'auto',
  overflowX: 'hidden',
  background: 'var(--bg-bar)',
  border: '1px solid var(--border)',
  borderRadius: 8,
  padding: 10,
  boxShadow: '0 8px 24px rgba(0, 0, 0, 0.5)'
}

const searchStyle: React.CSSProperties = {
  width: '100%',
  padding: '6px 8px',
  background: 'var(--surface-2)',
  border: '1px solid var(--border-2)',
  borderRadius: 6,
  color: 'var(--text)',
  fontSize: 12
}

const popoverHeader: React.CSSProperties = {
  display: 'flex',
  justifyContent: 'flex-end',
  padding: '6px 2px 8px'
}

const groupHeader: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: 6,
  padding: '0 2px 6px'
}

const groupIcon: React.CSSProperties = {
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  width: 16,
  height: 16,
  borderRadius: 4,
  background: 'var(--surface-3)',
  fontSize: 10,
  fontWeight: 700
}

const modelRow: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  width: '100%',
  padding: '7px 10px',
  background: 'transparent',
  border: '1px solid transparent',
  borderRadius: 6,
  color: 'var(--text)',
  fontSize: 12,
  cursor: 'pointer',
  textAlign: 'left'
}

const modelRowActive: React.CSSProperties = {
  ...modelRow,
  background: 'var(--accent-soft)',
  border: '1px solid var(--accent-border)',
  color: 'var(--accent-text)',
  fontWeight: 600
}

const hintRow: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: 6,
  flexWrap: 'wrap'
}

const codeChip: React.CSSProperties = {
  flex: 1,
  minWidth: 0,
  padding: '3px 6px',
  background: 'var(--surface-2)',
  border: '1px solid var(--border-2)',
  borderRadius: 4,
  fontSize: 10,
  fontFamily: 'ui-monospace, monospace',
  overflowX: 'auto',
  whiteSpace: 'nowrap'
}

const keyInputStyle: React.CSSProperties = {
  flex: 1,
  minWidth: 0,
  padding: '5px 8px',
  background: 'var(--surface-2)',
  border: '1px solid var(--border-2)',
  borderRadius: 6,
  color: 'var(--text)',
  fontSize: 12
}

const saveBtn: React.CSSProperties = {
  padding: '5px 12px',
  background: 'var(--surface-3)',
  border: '1px solid var(--border-2)',
  borderRadius: 6,
  color: 'var(--text)',
  fontSize: 12,
  cursor: 'pointer'
}

const linkBtn: React.CSSProperties = {
  background: 'none',
  border: 'none',
  color: 'var(--text-2)',
  fontSize: 11,
  cursor: 'pointer',
  textDecoration: 'underline',
  padding: 0
}
