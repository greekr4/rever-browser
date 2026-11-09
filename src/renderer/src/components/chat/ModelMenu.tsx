import { useEffect, useMemo, useRef, useState } from 'react'

import type { ACPAgentID } from '@/constants'
import { useT } from '@/stores/i18n'
import type { TKey } from '@/locales/en'
import { ProviderIcon } from './ProviderIcon'

export interface MenuModel {
  modelId: string
  name: string
  description?: string | null
  effortLevels?: string[]
  supportsFastMode?: boolean
}

const EFFORT_KEY: Record<string, TKey> = {
  low: 'model.effort.low',
  medium: 'model.effort.medium',
  high: 'model.effort.high',
  xhigh: 'model.effort.xhigh',
  max: 'model.effort.max'
}

/**
 * The composer's model chip for ONE agent (the tab's): search, pick a model,
 * and — when the agent reports them — reasoning effort and fast mode.
 */
export function ModelMenu({
  agentId,
  models,
  currentModelId,
  label,
  effort,
  fastMode,
  live,
  disabled,
  onModel,
  onEffort,
  onFastMode
}: {
  agentId: ACPAgentID
  models: MenuModel[]
  currentModelId: string | null
  /** Chip text: the model name (falls back to the agent name). */
  label: string
  effort: string | null
  fastMode: boolean
  /** Effort / fast mode can only change on a running session. */
  live: boolean
  disabled?: boolean
  onModel: (m: MenuModel) => void
  onEffort: (level: string) => void
  onFastMode: (on: boolean) => void
}) {
  const tr = useT()
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [pos, setPos] = useState({ bottom: 0, right: 0, width: 320 })
  const btnRef = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') setOpen(false)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [open])

  const toggle = (): void => {
    if (!open && btnRef.current) {
      // Fixed, not absolute: the chat column clips overflow, which cut the old popover off.
      const r = btnRef.current.getBoundingClientRect()
      const panel = btnRef.current.closest('form')?.getBoundingClientRect()
      setPos({
        bottom: window.innerHeight - r.top + 6,
        right: window.innerWidth - (panel?.right ?? r.right),
        width: Math.min(340, (panel?.width ?? 340))
      })
      setQuery('')
    }
    setOpen(!open)
  }

  const current = models.find((m) => m.modelId === currentModelId)
  const levels = current?.effortLevels ?? []
  const shown = useMemo(() => {
    const q = query.trim().toLowerCase()
    return q ? models.filter((m) => `${m.name} ${m.description ?? ''}`.toLowerCase().includes(q)) : models
  }, [models, query])
  const effortText = effort && EFFORT_KEY[effort] ? tr(EFFORT_KEY[effort]) : null

  return (
    <>
      <button
        ref={btnRef}
        type="button"
        className="composer-chip"
        data-testid="model-chip"
        aria-haspopup="dialog"
        aria-expanded={open}
        title={tr('model.chipTitle')}
        disabled={disabled}
        onClick={toggle}
      >
        <ProviderIcon agentId={agentId} size={13} />
        <span>
          {label}
          {effortText && <span style={{ color: 'var(--text-dim)' }}> {effortText}</span>}
          {fastMode && <span style={{ color: 'var(--text-dim)' }}> ⚡</span>}
        </span>
        <span aria-hidden="true" style={{ fontSize: 9, opacity: 0.7 }}>▾</span>
      </button>
      {open && (
        <>
          <div style={{ position: 'fixed', inset: 0, zIndex: 49 }} onClick={() => setOpen(false)} />
          <div
            role="dialog"
            aria-label={tr('model.chipTitle')}
            className="chrome-menu model-menu"
            data-testid="model-menu"
            style={{ bottom: pos.bottom, right: pos.right, width: pos.width }}
          >
            <div className="model-menu-search">
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" aria-hidden="true">
                <circle cx="11" cy="11" r="7" />
                <path d="M21 21l-4.3-4.3" />
              </svg>
              <input autoFocus value={query} onChange={(e) => setQuery(e.target.value)} placeholder={tr('model.search')} />
            </div>
            <div className="model-menu-list">
              {shown.map((m) => (
                <button
                  key={m.modelId}
                  type="button"
                  role="menuitemradio"
                  aria-checked={m.modelId === currentModelId}
                  className="model-menu-row"
                  onClick={() => {
                    onModel(m)
                    setOpen(false)
                  }}
                >
                  <span style={{ display: 'flex', flexDirection: 'column', gap: 2, minWidth: 0 }}>
                    <span>{m.name}</span>
                    {m.description && <span className="composer-menu-hint">{m.description}</span>}
                  </span>
                  {m.modelId === currentModelId && <span style={{ marginLeft: 'auto' }}>✓</span>}
                </button>
              ))}
              {shown.length === 0 && <div className="composer-menu-hint" style={{ padding: '8px 14px' }}>{tr('model.noMatch')}</div>}
            </div>
            {(levels.length > 0 || current?.supportsFastMode) && <div className="chrome-menu-sep" />}
            {levels.length > 0 && (
              <div className="model-menu-option" title={live ? undefined : tr('model.optionsAfterStart')}>
                <span>{tr('model.effort')}</span>
                <div role="radiogroup" aria-label={tr('model.effort')} className="effort-seg">
                  {levels.map((l) => (
                    <button
                      key={l}
                      type="button"
                      role="radio"
                      aria-checked={l === effort}
                      disabled={!live}
                      onClick={() => onEffort(l)}
                    >
                      {EFFORT_KEY[l] ? tr(EFFORT_KEY[l]) : l}
                    </button>
                  ))}
                </div>
              </div>
            )}
            {current?.supportsFastMode && (
              <label className="model-menu-option" title={live ? undefined : tr('model.optionsAfterStart')}>
                <span>{tr('model.fastMode')}</span>
                <input
                  type="checkbox"
                  role="switch"
                  className="switch"
                  checked={fastMode}
                  disabled={!live}
                  onChange={(e) => onFastMode(e.target.checked)}
                />
              </label>
            )}
            {!live && (levels.length > 0 || current?.supportsFastMode) && (
              <div className="chrome-menu-foot">{tr('model.optionsAfterStart')}</div>
            )}
          </div>
        </>
      )}
    </>
  )
}
