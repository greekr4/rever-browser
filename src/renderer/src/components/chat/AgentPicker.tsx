import { useEffect, useMemo, useRef, useState } from 'react'

import { ACP_AGENTS, type ACPAgentDef, type ACPAgentID } from '@/constants'
import type { TKey } from '@/locales/en'
import { useT } from '@/stores/i18n'

import type { AgentHealth, AgentHealthStatus } from '../../../../preload'

// provider list. The compact trigger shows the selected agent
// inline; opening reveals a vertical list of every catalog entry with a live
// health badge (real ACP handshake / authenticated API check via acp.probe),
// an "connected automatically" row when an existing CLI login covers it, plus
// install/sign-in hints and API-key inputs. Only `ready` rows are selectable.

type KeyProvider = 'anthropic' | 'openai'

function isKeyProvider(def: ACPAgentDef): def is ACPAgentDef & { provider: KeyProvider } {
  return def.provider === 'anthropic' || def.provider === 'openai'
}

// Sentinel path handed to onChange for API providers (no PATH binary). The
// router dispatches on the agent id, so the value itself is unused — it only
// needs to be non-null so the parent treats the pick as resolved.
const API_SENTINEL = 'api'

const PROBE_DEFS = ACP_AGENTS.map((a) => ({
  id: a.id,
  provider: a.provider,
  command: a.command,
  fallbackBins: a.fallbackBins
}))

const STATUS_COLOR: Record<AgentHealthStatus, string> = {
  ready: 'var(--status-ok)',
  'not-installed': 'var(--http-none)',
  'needs-key': 'var(--status-warn)',
  'needs-login': 'var(--status-warn)',
  'auth-failed': 'var(--status-error)',
  failed: 'var(--status-error)'
}

interface AgentPickerProps {
  agentId: ACPAgentID
  onChange: (id: ACPAgentID, resolvedPath: string) => void
  disabled?: boolean
}

export function AgentPicker({ agentId, onChange, disabled }: AgentPickerProps) {
  const tr = useT()
  const [open, setOpen] = useState(false)
  const [health, setHealth] = useState<Record<string, AgentHealth>>({})
  const [loading, setLoading] = useState(true)
  const [keyInput, setKeyInput] = useState<Record<string, string>>({})
  const [savingKey, setSavingKey] = useState<string | null>(null)
  const [copied, setCopied] = useState<string | null>(null)
  const popoverRef = useRef<HTMLDivElement>(null)
  const triggerRef = useRef<HTMLButtonElement>(null)

  const scan = async () => {
    setLoading(true)
    const results = await window.rev.acp.probe(PROBE_DEFS)
    setHealth(Object.fromEntries(results.map((r) => [r.id, r])))
    setLoading(false)
  }

  useEffect(() => {
    let cancelled = false
    void window.rev.acp.probe(PROBE_DEFS).then((results) => {
      if (cancelled) return
      setHealth(Object.fromEntries(results.map((r) => [r.id, r])))
      setLoading(false)
    })
    return () => {
      cancelled = true
    }
  }, [])

  const selectedDef = ACP_AGENTS.find((a) => a.id === agentId) ?? ACP_AGENTS[0]
  const selectedStatus = health[selectedDef.id]?.status ?? 'failed'

  const readyCount = useMemo(
    () => Object.values(health).filter((h) => h.status === 'ready').length,
    [health]
  )

  // Resolve the current pick's path (or auto-switch to the first ready agent)
  // once probing finishes, mirroring the pre-rewrite behaviour so a machine
  // without the default binary lands on a working selection.
  useEffect(() => {
    if (loading) return
    const cur = health[selectedDef.id]
    if (cur?.status === 'ready') {
      onChange(selectedDef.id, resolvedPathFor(selectedDef, cur))
      return
    }
    const firstReady = ACP_AGENTS.find((d) => health[d.id]?.status === 'ready')
    if (firstReady) onChange(firstReady.id, resolvedPathFor(firstReady, health[firstReady.id]))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loading, health])

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

  const pick = (def: ACPAgentDef) => {
    const h = health[def.id]
    if (h?.status !== 'ready') return
    onChange(def.id, resolvedPathFor(def, h))
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
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen((v) => !v)}
        disabled={disabled}
        style={triggerStyle}
        title={tr('chat.chooseAgent')}
      >
        <span style={iconChip}>{selectedDef.icon}</span>
        <span style={{ fontWeight: 500 }}>{selectedDef.name}</span>
        <span style={{ ...statusDot, background: STATUS_COLOR[selectedStatus] }} />
        <span style={{ opacity: 0.6, fontSize: 10 }}>▾</span>
      </button>

      {open && (
        <div ref={popoverRef} style={popoverStyle}>
          <header style={popoverHeader}>
            <strong style={{ fontSize: 12 }}>{tr('chat.chooseAgentShort')}</strong>
            <span style={{ fontSize: 11, opacity: 0.6 }}>
              {loading ? tr('onboard.scanning') : tr('onboard.ready', { n: readyCount })}
            </span>
          </header>

          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            {ACP_AGENTS.map((def) => {
              const h = health[def.id]
              const status = h?.status ?? 'failed'
              const ready = status === 'ready'
              const active = def.id === agentId
              const label = h?.auto
                ? tr('onboard.status.auto')
                : tr(`onboard.status.${status}` as TKey)
              const hint =
                status === 'not-installed'
                  ? { title: tr('onboard.installWith'), cmd: def.installHint }
                  : status === 'needs-login' && def.loginHint
                    ? { title: tr('onboard.signInWith'), cmd: def.loginHint }
                    : null

              return (
                <div
                  key={def.id}
                  style={{
                    ...row,
                    opacity: loading && !h ? 0.5 : 1,
                    borderColor: active ? 'var(--accent-border)' : 'var(--border)'
                  }}
                >
                  <div style={rowTop}>
                    <span style={rowIcon}>{def.icon}</span>
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div style={{ fontSize: 13, fontWeight: 500 }}>{def.name}</div>
                      <div style={{ fontSize: 11, color: STATUS_COLOR[status] }}>
                        {label}
                        {h?.plan && h.plan !== 'api-key'
                          ? ` · ${tr('onboard.plan', { plan: h.plan })}`
                          : ''}
                        {h?.detail ? ` · ${h.detail}` : ''}
                      </div>
                    </div>
                    <button
                      type="button"
                      onClick={() => pick(def)}
                      disabled={!ready}
                      style={ready ? (active ? useBtnActive : useBtn) : useBtnDisabled}
                    >
                      {active ? `✓ ${tr('onboard.inUse')}` : tr('onboard.use')}
                    </button>
                  </div>

                  {hint && (
                    <div style={hintRow}>
                      <span style={{ fontSize: 10, color: 'var(--text-2)' }}>{hint.title}</span>
                      <code style={codeChip}>{hint.cmd}</code>
                      <button type="button" onClick={() => copy(hint.cmd)} style={linkBtn}>
                        {copied === hint.cmd ? tr('onboard.copied') : tr('onboard.copy')}
                      </button>
                    </div>
                  )}

                  {isKeyProvider(def) && status !== 'ready' && (
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
              )
            })}
          </div>
        </div>
      )}
    </div>
  )
}

function resolvedPathFor(def: ACPAgentDef, h: AgentHealth | undefined): string {
  if (isKeyProvider(def)) return API_SENTINEL
  return h?.resolvedPath ?? def.command
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

const statusDot: React.CSSProperties = {
  width: 6,
  height: 6,
  borderRadius: '50%'
}

const popoverStyle: React.CSSProperties = {
  position: 'absolute',
  top: 'calc(100% + 6px)',
  left: 0,
  zIndex: 50,
  width: 'min(340px, calc(var(--chat-w, 100vw) - 24px))',
  maxHeight: '70vh',
  overflowY: 'auto',
  background: 'var(--bg-bar)',
  border: '1px solid var(--border)',
  borderRadius: 8,
  padding: 10,
  boxShadow: '0 8px 24px rgba(0, 0, 0, 0.5)'
}

const popoverHeader: React.CSSProperties = {
  display: 'flex',
  justifyContent: 'space-between',
  alignItems: 'center',
  padding: '2px 4px 10px'
}

const row: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: 8,
  padding: 10,
  background: 'var(--surface-2)',
  border: '1px solid var(--border)',
  borderRadius: 8
}

const rowTop: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: 10
}

const rowIcon: React.CSSProperties = {
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  width: 28,
  height: 28,
  borderRadius: 8,
  background: 'var(--surface-3)',
  fontSize: 13,
  fontWeight: 700,
  flexShrink: 0
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
  background: 'var(--bg-bar)',
  border: '1px solid var(--border-2)',
  borderRadius: 4,
  fontSize: 10,
  fontFamily: 'ui-monospace, monospace',
  overflowX: 'auto',
  whiteSpace: 'nowrap'
}

// WCAG AA: accent-text (not --accent) clears 4.5:1 on the soft/card backgrounds
// in both themes; white-on-accent fails in dark, so the active state keeps the
// same readable foreground and marks selection with a solid ring instead.
const useBtn: React.CSSProperties = {
  padding: '5px 12px',
  background: 'var(--accent-soft)',
  border: '1px solid var(--accent-border)',
  borderRadius: 6,
  color: 'var(--accent-text)',
  fontSize: 12,
  flexShrink: 0,
  cursor: 'pointer'
}

const useBtnActive: React.CSSProperties = {
  ...useBtn,
  border: '2px solid var(--accent)',
  fontWeight: 600
}

const useBtnDisabled: React.CSSProperties = {
  ...useBtn,
  background: 'var(--surface-3)',
  border: '1px solid var(--border)',
  color: 'var(--text-2)',
  cursor: 'not-allowed'
}

const keyInputStyle: React.CSSProperties = {
  flex: 1,
  minWidth: 0,
  padding: '5px 8px',
  background: 'var(--bg-bar)',
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
