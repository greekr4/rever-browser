import { useEffect, useMemo, useRef, useState } from 'react'

import { ACP_AGENTS, type ACPAgentDef, type ACPAgentID } from '@/constants'

export interface AgentDetection {
  /** Map keyed by the catalog `command`. */
  resolved: Record<string, string | null>
}

interface AgentTileInfo {
  def: ACPAgentDef
  resolvedPath: string | null
  detected: boolean
  selectable: boolean
  status: 'ready' | 'not-installed' | 'needs-key' | 'unsupported'
}

// Providers gated on an API key rather than a PATH binary.
type KeyProvider = 'anthropic' | 'openai'
const KEY_PROVIDERS: { id: KeyProvider; label: string }[] = [
  { id: 'anthropic', label: 'Anthropic API key' },
  { id: 'openai', label: 'OpenAI API key' }
]

// Sentinel passed to onChange for API providers, which have no PATH binary —
// the router dispatches on the agent id, so the value is unused.
const API_SENTINEL = 'api'

function buildTiles(
  detection: AgentDetection,
  apiKeys: Record<KeyProvider, boolean>
): AgentTileInfo[] {
  return ACP_AGENTS.map((def) => {
    if (def.provider === 'anthropic' || def.provider === 'openai') {
      const hasKey = apiKeys[def.provider]
      return {
        def,
        resolvedPath: hasKey ? API_SENTINEL : null,
        detected: hasKey,
        selectable: hasKey,
        status: hasKey ? 'ready' : 'needs-key'
      }
    }
    const resolvedPath = detection.resolved[def.command] ?? null
    const detected = resolvedPath !== null
    const selectable = def.acpSupported && detected
    const status: AgentTileInfo['status'] = !def.acpSupported
      ? 'unsupported'
      : detected
        ? 'ready'
        : 'not-installed'
    return { def, resolvedPath, detected, selectable, status }
  })
}

const STATUS_LABEL: Record<AgentTileInfo['status'], string> = {
  ready: 'Ready',
  'not-installed': 'Not installed',
  'needs-key': 'Needs API key',
  unsupported: 'Not yet ACP-compatible'
}

const STATUS_COLOR: Record<AgentTileInfo['status'], string> = {
  ready: '#3aa55d',
  'not-installed': '#888',
  'needs-key': '#c98a3a',
  unsupported: '#c98a3a'
}

interface AgentPickerProps {
  agentId: ACPAgentID
  onChange: (id: ACPAgentID, resolvedPath: string) => void
  disabled?: boolean
}

/**
 * Compact open-design-style agent picker. Renders the selected tile inline;
 * clicking opens a 4-column grid of all catalog entries with detection
 * badges. Only ACP-supported & detected tiles are selectable.
 */
export function AgentPicker({ agentId, onChange, disabled }: AgentPickerProps) {
  const [open, setOpen] = useState(false)
  const [detection, setDetection] = useState<AgentDetection>({ resolved: {} })
  const [loading, setLoading] = useState(true)
  const [apiKeys, setApiKeys] = useState<Record<KeyProvider, boolean>>({
    anthropic: false,
    openai: false
  })
  const [keyInput, setKeyInput] = useState<Record<KeyProvider, string>>({
    anthropic: '',
    openai: ''
  })
  const [savingKey, setSavingKey] = useState<KeyProvider | null>(null)
  const popoverRef = useRef<HTMLDivElement>(null)
  const triggerRef = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    let cancelled = false
    const probes = ACP_AGENTS.filter(
      (a) => a.provider !== 'anthropic' && a.provider !== 'openai' && a.command
    ).map((a) => ({ command: a.command, fallbackBins: a.fallbackBins }))
    void Promise.all([
      window.rev.acp.listAvailable(probes),
      window.rev.settings.hasApiKey('anthropic'),
      window.rev.settings.hasApiKey('openai')
    ]).then(([results, anthropicKey, openaiKey]) => {
      if (cancelled) return
      const resolved: Record<string, string | null> = {}
      for (const r of results) resolved[r.command] = r.resolvedPath
      setDetection({ resolved })
      setApiKeys({ anthropic: anthropicKey, openai: openaiKey })
      setLoading(false)
    })
    return () => {
      cancelled = true
    }
  }, [])

  const saveApiKey = async (provider: KeyProvider) => {
    const key = keyInput[provider].trim()
    if (!key) return
    setSavingKey(provider)
    try {
      const ok = await window.rev.settings.setApiKey(provider, key)
      setApiKeys((prev) => ({ ...prev, [provider]: ok }))
      if (ok) setKeyInput((prev) => ({ ...prev, [provider]: '' }))
    } finally {
      setSavingKey(null)
    }
  }

  const tiles = useMemo(() => buildTiles(detection, apiKeys), [detection, apiKeys])
  const selected = tiles.find((t) => t.def.id === agentId) ?? tiles[0]

  // Push the resolved absolute path to the parent once detection finishes.
  // Two cases:
  //   1. Currently selected agent is ready → seed agentBinPath so the spawn
  //      call doesn't fall back to the bare command name (ENOENT on Windows
  //      .cmd shims, fragile on macOS shells that don't propagate PATH).
  //   2. Currently selected agent is unselectable → switch to first ready.
  useEffect(() => {
    if (loading) return
    if (selected.selectable && selected.resolvedPath) {
      onChange(selected.def.id, selected.resolvedPath)
      return
    }
    const firstReady = tiles.find((t) => t.selectable)
    if (firstReady && firstReady.resolvedPath) {
      onChange(firstReady.def.id, firstReady.resolvedPath)
    }
  }, [loading, selected, tiles, onChange])

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

  const pickTile = (tile: AgentTileInfo) => {
    if (!tile.selectable || !tile.resolvedPath) return
    onChange(tile.def.id, tile.resolvedPath)
    setOpen(false)
  }

  return (
    <div style={{ position: 'relative' }}>
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen((v) => !v)}
        disabled={disabled}
        style={triggerStyle}
        title="Choose AI agent"
      >
        <span style={iconChip}>{selected.def.icon}</span>
        <span style={{ fontWeight: 500 }}>{selected.def.name}</span>
        <span style={{ ...statusDot, background: STATUS_COLOR[selected.status] }} />
        <span style={{ opacity: 0.6, fontSize: 10 }}>▾</span>
      </button>

      {open && (
        <div ref={popoverRef} style={popoverStyle}>
          <header style={popoverHeader}>
            <strong style={{ fontSize: 12 }}>Choose agent</strong>
            <span style={{ fontSize: 11, opacity: 0.6 }}>
              {loading ? 'Scanning PATH…' : `${tiles.filter((t) => t.selectable).length} ready`}
            </span>
          </header>
          <div style={gridStyle}>
            {tiles.map((tile) => (
              <button
                key={tile.def.id}
                type="button"
                onClick={() => pickTile(tile)}
                disabled={!tile.selectable}
                title={
                  tile.selectable
                    ? `${tile.def.name}\n${tile.resolvedPath}`
                    : `${STATUS_LABEL[tile.status]} — ${tile.def.installHint}`
                }
                style={{
                  ...tileStyle,
                  opacity: tile.selectable ? 1 : 0.45,
                  cursor: tile.selectable ? 'pointer' : 'not-allowed',
                  borderColor: tile.def.id === agentId ? '#4a8ddb' : 'var(--border)'
                }}
              >
                <div style={tileIconChip}>{tile.def.icon}</div>
                <div
                  style={{
                    fontSize: 11,
                    fontWeight: 500,
                    textAlign: 'center',
                    wordBreak: 'break-word',
                    lineHeight: 1.2
                  }}
                >
                  {tile.def.name}
                </div>
                <div
                  style={{
                    fontSize: 9,
                    color: STATUS_COLOR[tile.status],
                    textTransform: 'uppercase',
                    letterSpacing: 0.3,
                    wordBreak: 'break-word',
                    lineHeight: 1.2
                  }}
                >
                  {STATUS_LABEL[tile.status]}
                </div>
              </button>
            ))}
          </div>
          {KEY_PROVIDERS.map((kp) => (
            <div key={kp.id} style={keyRowStyle}>
              <label style={{ fontSize: 11, opacity: 0.7 }}>
                {kp.label} {apiKeys[kp.id] ? '· saved' : '· not set'}
              </label>
              <div style={{ display: 'flex', gap: 6 }}>
                <input
                  type="password"
                  value={keyInput[kp.id]}
                  onChange={(e) =>
                    setKeyInput((prev) => ({ ...prev, [kp.id]: e.target.value }))
                  }
                  placeholder={
                    apiKeys[kp.id] ? 'Replace key…' : kp.id === 'openai' ? 'sk-…' : 'sk-ant-…'
                  }
                  style={keyInputStyle}
                />
                <button
                  type="button"
                  onClick={() => void saveApiKey(kp.id)}
                  disabled={savingKey === kp.id || !keyInput[kp.id].trim()}
                  style={keySaveStyle}
                >
                  {savingKey === kp.id ? '…' : 'Save'}
                </button>
              </div>
            </div>
          ))}
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
  // Shrink to fit the chat panel (--chat-w is set on .agent-panel in App.tsx)
  // so we don't overflow past the panel's right edge when the user makes the
  // chat panel narrow. Fallback to 100vw when --chat-w isn't available.
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
  padding: '2px 4px 8px'
}

const gridStyle: React.CSSProperties = {
  display: 'grid',
  // Reflow columns based on available width: ~3 columns at 340px, 2 columns
  // when the chat panel is narrow (~220px), so tiles never get clipped.
  gridTemplateColumns: 'repeat(auto-fill, minmax(86px, 1fr))',
  gap: 6
}

const tileStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  alignItems: 'center',
  justifyContent: 'flex-start',
  gap: 4,
  padding: '10px 4px',
  minHeight: 100,
  background: 'var(--bg-bar)',
  border: '1px solid var(--border)',
  borderRadius: 6,
  color: 'var(--text)',
  textAlign: 'center',
  minWidth: 0,
  overflow: 'hidden'
}

const keyRowStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: 6,
  marginTop: 10,
  paddingTop: 10,
  borderTop: '1px solid var(--border)'
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

const keySaveStyle: React.CSSProperties = {
  padding: '5px 12px',
  background: 'var(--surface-2)',
  border: '1px solid var(--border-2)',
  borderRadius: 6,
  color: 'var(--text)',
  fontSize: 12,
  cursor: 'pointer'
}

const tileIconChip: React.CSSProperties = {
  width: 32,
  height: 32,
  borderRadius: 8,
  background: 'var(--surface-2)',
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'center',
  fontSize: 16,
  fontWeight: 700
}
