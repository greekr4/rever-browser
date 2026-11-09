import { useEffect, useState, type ReactNode } from 'react'

export interface ChipOption<T extends string> {
  value: T
  label: string
  hint?: string
  testId?: string
}

/** A flat chip in the composer that opens its options upward (it sits at the bottom of the panel). */
export function ChipSelect<T extends string>({
  icon,
  value,
  options,
  onChange,
  title,
  testId,
  disabled
}: {
  icon?: ReactNode
  value: T
  options: ChipOption<T>[]
  onChange: (v: T) => void
  title: string
  testId?: string
  disabled?: boolean
}) {
  const [open, setOpen] = useState(false)
  const current = options.find((o) => o.value === value)

  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') setOpen(false)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [open])

  return (
    <span style={{ position: 'relative', display: 'inline-flex' }}>
      <button
        type="button"
        className="composer-chip collapsible"
        data-testid={testId}
        title={title}
        aria-label={`${title}: ${current?.label ?? value}`}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => setOpen(!open)}
      >
        {icon}
        <span>{current?.label ?? value}</span>
        <span aria-hidden="true" style={{ fontSize: 9, opacity: 0.7 }}>▾</span>
      </button>
      {open && (
        <>
          <div style={{ position: 'fixed', inset: 0, zIndex: 49 }} onClick={() => setOpen(false)} />
          <div role="menu" className="chrome-menu composer-menu">
            <div className="chrome-menu-foot" style={{ padding: '2px 14px 6px' }}>{title}</div>
            {options.map((o) => (
              <button
                key={o.value}
                type="button"
                role="menuitemradio"
                aria-checked={o.value === value}
                data-testid={o.testId}
                className="chrome-menu-item composer-menu-item"
                onClick={() => {
                  onChange(o.value)
                  setOpen(false)
                }}
              >
                <span style={{ display: 'flex', flexDirection: 'column', gap: 2, minWidth: 0 }}>
                  <span>{o.label}</span>
                  {o.hint && <span className="composer-menu-hint">{o.hint}</span>}
                </span>
                {o.value === value && <span style={{ marginLeft: 'auto', color: 'var(--accent-text)' }}>✓</span>}
              </button>
            ))}
          </div>
        </>
      )}
    </span>
  )
}
