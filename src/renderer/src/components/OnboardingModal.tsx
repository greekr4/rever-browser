import { useCallback, useEffect, useState } from 'react'

import { ACP_AGENTS, type ACPAgentDef, type ACPAgentID } from '@/constants'
import type { TKey } from '@/locales/en'
import { hasOnboarded, useAgentChoice } from '@/stores/agent-choice'
import { useT } from '@/stores/i18n'

import type { AgentHealth, AgentHealthStatus } from '../../../preload'
import { ProviderIcon } from './chat/ProviderIcon'

// First-run provider setup. Probes every agent for real (ACP handshake /
// authenticated API call) so a green row means it actually works, and shows
// "connected automatically" when an existing CLI login already covers it.

const STATUS_COLOR: Record<AgentHealthStatus, string> = {
  ready: 'var(--status-ok)',
  'not-installed': 'var(--http-none)',
  'needs-key': 'var(--status-warn)',
  'needs-login': 'var(--status-warn)',
  'auth-failed': 'var(--status-error)',
  failed: 'var(--status-error)'
}

type KeyProvider = 'anthropic' | 'openai'

function isKeyProvider(def: ACPAgentDef): def is ACPAgentDef & { provider: KeyProvider } {
  return def.provider === 'anthropic' || def.provider === 'openai'
}

const PROBE_DEFS = ACP_AGENTS.map((a) => ({
  id: a.id,
  provider: a.provider,
  command: a.command,
  fallbackBins: a.fallbackBins
}))

export function OnboardingModal() {
  const tr = useT()
  const pick = useAgentChoice((s) => s.pick)
  const skip = useAgentChoice((s) => s.skip)
  const [visible, setVisible] = useState(() => !hasOnboarded())
  const [health, setHealth] = useState<Record<string, AgentHealth>>({})
  const [scanning, setScanning] = useState(true)
  const [keyInput, setKeyInput] = useState<Record<string, string>>({})
  const [savingKey, setSavingKey] = useState<string | null>(null)
  const [copied, setCopied] = useState<string | null>(null)

  const scan = useCallback(async () => {
    setScanning(true)
    const results = await window.rev.acp.probe(PROBE_DEFS)
    setHealth(Object.fromEntries(results.map((r) => [r.id, r])))
    setScanning(false)
  }, [])

  useEffect(() => {
    if (!visible) return
    void scan()
  }, [visible, scan])

  if (!visible) return null

  const readyCount = Object.values(health).filter((h) => h.status === 'ready').length

  const choose = (def: ACPAgentDef) => {
    pick(def.id as ACPAgentID, health[def.id]?.resolvedPath ?? null)
    setVisible(false)
  }

  const onSkip = () => {
    skip()
    setVisible(false)
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
    <div style={backdrop} role="dialog" aria-modal="true" aria-label={tr('onboard.title')}>
      <div style={card}>
        <h2 style={{ margin: 0, fontSize: 18 }}>{tr('onboard.title')}</h2>
        <p style={{ margin: '6px 0 0', fontSize: 12, color: 'var(--text-2)', lineHeight: 1.5 }}>
          {tr('onboard.desc')}
        </p>

        <div style={statusRow}>
          <span style={{ fontSize: 11, color: 'var(--text-2)' }}>
            {scanning ? tr('onboard.scanning') : tr('onboard.ready', { n: readyCount })}
          </span>
          <button type="button" onClick={() => void scan()} disabled={scanning} style={linkBtn}>
            {tr('onboard.recheck')}
          </button>
        </div>

        <div style={{ display: 'flex', flexDirection: 'column', gap: 8, marginTop: 8 }}>
          {ACP_AGENTS.map((def) => {
            const h = health[def.id]
            const status = h?.status ?? 'failed'
            const ready = status === 'ready'
            const label = h?.auto ? tr('onboard.status.auto') : tr(`onboard.status.${status}` as TKey)
            const hint =
              status === 'not-installed'
                ? { title: tr('onboard.installWith'), cmd: def.installHint }
                : status === 'needs-login' && def.loginHint
                  ? { title: tr('onboard.signInWith'), cmd: def.loginHint }
                  : null

            return (
              <div key={def.id} style={{ ...row, opacity: scanning && !h ? 0.5 : 1 }}>
                <div style={rowTop}>
                  <span style={iconChip}>
                    <ProviderIcon agentId={def.id} size={16} fallback={def.icon} />
                  </span>
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
                    onClick={() => choose(def)}
                    disabled={!ready}
                    style={ready ? useBtn : useBtnDisabled}
                  >
                    {tr('onboard.use')}
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
                      disabled={savingKey === def.provider || !(keyInput[def.provider] ?? '').trim()}
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

        <div style={{ display: 'flex', justifyContent: 'flex-end', marginTop: 14 }}>
          <button type="button" onClick={onSkip} style={linkBtn}>
            {tr('onboard.skip')}
          </button>
        </div>
      </div>
    </div>
  )
}

const backdrop: React.CSSProperties = {
  position: 'fixed',
  inset: 0,
  zIndex: 9000,
  background: 'rgba(0, 0, 0, 0.6)',
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'center',
  padding: 16
}

const card: React.CSSProperties = {
  width: 'min(460px, 100%)',
  maxHeight: 'calc(100vh - 32px)',
  overflowY: 'auto',
  background: 'var(--bg-bar)',
  border: '1px solid var(--border)',
  borderRadius: 12,
  padding: 20,
  color: 'var(--text)',
  boxShadow: '0 16px 48px rgba(0, 0, 0, 0.55)'
}

const statusRow: React.CSSProperties = {
  display: 'flex',
  justifyContent: 'space-between',
  alignItems: 'center',
  marginTop: 16,
  paddingBottom: 6,
  borderBottom: '1px solid var(--border)'
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

const iconChip: React.CSSProperties = {
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  width: 30,
  height: 30,
  borderRadius: 8,
  background: 'var(--surface-3)',
  fontSize: 14,
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

// WCAG AA: --accent (#2f6fed) is only 4.02:1 on the light card; --accent-text
// clears 4.5:1 in both themes. Disabled uses a muted surface rather than a
// low-opacity accent (which dropped to ~1.7:1).
const useBtn: React.CSSProperties = {
  padding: '5px 14px',
  background: 'var(--accent-soft)',
  border: '1px solid var(--accent-border)',
  borderRadius: 6,
  color: 'var(--accent-text)',
  fontSize: 12,
  flexShrink: 0,
  cursor: 'pointer'
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
