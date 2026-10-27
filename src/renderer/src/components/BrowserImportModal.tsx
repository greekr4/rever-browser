import { useEffect, useState } from 'react'
import { create } from 'zustand'

import { useAgentChoice } from '@/stores/agent-choice'
import { useT } from '@/stores/i18n'

import type { BrowserId, BrowserInfo } from '../../../preload'
import { backdrop, card, linkBtn, useBtn } from './OnboardingModal'

// One-click "bring your logins over" import, Aside-style. Offered once after
// first-run onboarding (macOS only) and reopened from the app menu. Cookies go
// into the shared browsing session; reading the source browser's Safe Storage
// key makes macOS show its Keychain prompt. Saved passwords are never read.

const OFFERED_KEY = 'rev:browser-import-offered'

export const useBrowserImport = create<{ open: boolean }>(() => ({
  open: localStorage.getItem(OFFERED_KEY) === null && navigator.userAgent.includes('Mac')
}))

function defaultProfile(b: BrowserInfo | undefined): string {
  return b?.profiles.find((p) => p.id === 'Default')?.id ?? b?.profiles[0]?.id ?? ''
}

export function BrowserImportModal() {
  const tr = useT()
  const open = useBrowserImport((s) => s.open)
  // The first-run offer waits until the provider onboarding modal closes.
  const onboarded = useAgentChoice((s) => s.onboarded)
  const visible = open && onboarded
  const [browsers, setBrowsers] = useState<BrowserInfo[] | null>(null)
  const [browserId, setBrowserId] = useState<BrowserId | ''>('')
  const [profile, setProfile] = useState('')
  const [busy, setBusy] = useState(false)
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null)

  useEffect(() => {
    if (!visible) return
    setResult(null)
    void window.rev.storage.browsers().then((list) => {
      setBrowsers(list)
      setBrowserId(list[0]?.id ?? '')
      setProfile(defaultProfile(list[0]))
    })
  }, [visible])

  if (!visible) return null

  const selected = browsers?.find((b) => b.id === browserId)

  const close = (): void => {
    localStorage.setItem(OFFERED_KEY, '1')
    useBrowserImport.setState({ open: false })
  }

  const run = async (): Promise<void> => {
    if (!browserId) return
    setBusy(true)
    setResult(null)
    try {
      const r = await window.rev.storage.browserImport({ browser: browserId, profile, shared: true })
      if (!r.ok) {
        setResult({ ok: false, text: r.error ?? 'import failed' })
      } else {
        const parts = [tr('import.done', { n: r.imported.toLocaleString('en-US') })]
        if (r.undecryptable) {
          parts.push(tr('import.appBound', { n: r.undecryptable.toLocaleString('en-US') }))
        }
        setResult({ ok: true, text: parts.join(' · ') })
      }
    } catch (e) {
      setResult({ ok: false, text: e instanceof Error ? e.message : String(e) })
    } finally {
      setBusy(false)
    }
  }

  const done = result?.ok === true

  return (
    <div style={backdrop} role="dialog" aria-modal="true" aria-label={tr('import.title')}>
      <div style={card}>
        <h2 style={{ margin: 0, fontSize: 18 }}>{tr('import.title')}</h2>

        {browsers?.length === 0 ? (
          <p style={{ margin: '16px 0 0', fontSize: 12 }}>{tr('import.none')}</p>
        ) : (
          <div style={{ display: 'flex', gap: 8, marginTop: 16, flexWrap: 'wrap' }}>
            <label style={fieldLabel}>
              {tr('import.browser')}
              <select
                value={browserId}
                disabled={busy || !browsers}
                onChange={(e) => {
                  const id = e.target.value as BrowserId
                  setBrowserId(id)
                  setProfile(defaultProfile(browsers?.find((b) => b.id === id)))
                }}
                style={selectStyle}
              >
                {browsers?.map((b) => (
                  <option key={b.id} value={b.id}>
                    {b.name}
                  </option>
                ))}
              </select>
            </label>
            {selected && selected.profiles.length > 1 && (
              <label style={fieldLabel}>
                {tr('import.profile')}
                <select
                  value={profile}
                  disabled={busy}
                  onChange={(e) => setProfile(e.target.value)}
                  style={selectStyle}
                >
                  {selected.profiles.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </select>
              </label>
            )}
          </div>
        )}

        {result && (
          <p
            role="status"
            style={{
              margin: '12px 0 0',
              fontSize: 12,
              color: result.ok ? 'var(--status-ok)' : 'var(--status-error)'
            }}
          >
            {result.text}
          </p>
        )}

        <div style={{ display: 'flex', justifyContent: 'flex-end', alignItems: 'center', gap: 12, marginTop: 16 }}>
          <button type="button" onClick={close} disabled={busy} style={linkBtn}>
            {done ? tr('import.close') : tr('import.later')}
          </button>
          {!done && !!browsers?.length && (
            <button type="button" onClick={() => void run()} disabled={busy} style={useBtn}>
              {busy ? tr('import.running') : tr('import.run')}
            </button>
          )}
        </div>
      </div>
    </div>
  )
}

const fieldLabel: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: 4,
  fontSize: 11,
  color: 'var(--text-2)',
  flex: 1,
  minWidth: 140
}

const selectStyle: React.CSSProperties = {
  padding: '5px 8px',
  background: 'var(--bg-bar)',
  border: '1px solid var(--border-2)',
  borderRadius: 6,
  color: 'var(--text)',
  fontSize: 12
}
