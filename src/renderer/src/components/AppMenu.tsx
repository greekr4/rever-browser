import { useEffect, useRef, useState } from 'react'

export interface AppMenuItem {
  label: string
  /** Current value shown on the right (e.g. "Dark", "EN"). */
  hint?: string
  onClick: () => void
  disabled?: boolean
  title?: string
  /** Draw a separator above this item. */
  sep?: boolean
}

/** Chrome's ⋮ "Customize and control" menu: settings that don't need a permanent button. */
export function AppMenu({ items, title, footer }: { items: AppMenuItem[]; title: string; footer?: string }) {
  const [open, setOpen] = useState(false)
  const [pos, setPos] = useState({ top: 0, right: 0 })
  const btnRef = useRef<HTMLButtonElement>(null)

  // Window-level so Esc works even after focus left the menu.
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
      const r = btnRef.current.getBoundingClientRect()
      setPos({ top: r.bottom + 4, right: window.innerWidth - r.right })
    }
    setOpen(!open)
  }

  return (
    <>
      <button
        ref={btnRef}
        type="button"
        className="chrome-icon-btn"
        onClick={toggle}
        title={title}
        aria-label={title}
        aria-haspopup="menu"
        aria-expanded={open}
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <circle cx="12" cy="5" r="1.8" />
          <circle cx="12" cy="12" r="1.8" />
          <circle cx="12" cy="19" r="1.8" />
        </svg>
      </button>
      {open && (
        <>
          <div style={{ position: 'fixed', inset: 0, zIndex: 49 }} onClick={() => setOpen(false)} />
          <div
            role="menu"
            className="chrome-menu"
            style={{ top: pos.top, right: pos.right }}
          >
            {/* Index keys: labels change with the language, which would remount the focused item. */}
            {items.map((it, i) => (
              <div key={i}>
                {it.sep && <div className="chrome-menu-sep" />}
                <button
                  type="button"
                  role="menuitem"
                  className="chrome-menu-item"
                  disabled={it.disabled}
                  title={it.title}
                  // Menu stays open so cycling items (theme, language) can be clicked repeatedly.
                  onClick={it.onClick}
                >
                  <span>{it.label}</span>
                  {it.hint && <span className="hint">{it.hint}</span>}
                </button>
              </div>
            ))}
            {footer && (
              <>
                <div className="chrome-menu-sep" />
                <div className="chrome-menu-foot">{footer}</div>
              </>
            )}
          </div>
        </>
      )}
    </>
  )
}
