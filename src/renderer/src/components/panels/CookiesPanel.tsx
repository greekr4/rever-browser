import { useCallback, useEffect, useState } from 'react'

interface CookieRow {
  name: string
  value: string
  domain: string
  path: string
  expires?: number
  secure?: boolean
  httpOnly?: boolean
  sameSite?: string
}

type Section = 'cookies' | 'local' | 'session'

export function CookiesPanel() {
  const [section, setSection] = useState<Section>('cookies')
  const [cookies, setCookies] = useState<CookieRow[]>([])
  const [origin, setOrigin] = useState<string | null>(null)
  const [local, setLocal] = useState<Record<string, string>>({})
  const [session, setSession] = useState<Record<string, string>>({})
  const [editing, setEditing] = useState<{ key: string; value: string } | null>(null)
  const [adding, setAdding] = useState<{ key: string; value: string } | null>(null)
  const [editCookie, setEditCookie] = useState<CookieRow | null>(null)
  const [newCookie, setNewCookie] = useState<CookieRow | null>(null)
  const [sticky, setSticky] = useState<{ enabled: boolean; snapshotCount: number }>({
    enabled: false,
    snapshotCount: 0
  })
  const [dialog, setDialog] = useState<{
    autoDismiss: boolean
    history: Array<{ ts: number; type: string; message: string; url: string }>
  }>({ autoDismiss: true, history: [] })

  useEffect(() => {
    void window.rev.storage.persistenceGet().then(setSticky)
    void window.rev.dialog.getSettings().then(setDialog)
    const id = setInterval(() => {
      void window.rev.dialog.getSettings().then(setDialog)
    }, 3000)
    return () => clearInterval(id)
  }, [])

  const refresh = useCallback(async () => {
    const [c, l, s] = await Promise.all([
      window.rev.storage.cookies(),
      window.rev.storage.localGet(),
      window.rev.storage.sessionGet()
    ])
    setCookies(c.cookies as CookieRow[])
    setOrigin(c.origin)
    setLocal(l)
    setSession(s)
  }, [])

  useEffect(() => {
    void refresh()
    const id = setInterval(() => void refresh(), 3000)
    return () => clearInterval(id)
  }, [refresh])

  return (
    <div style={{ padding: 10, fontSize: 12, color: '#ddd', height: '100%', display: 'flex', flexDirection: 'column', minHeight: 0 }}>
      <div style={{ display: 'flex', gap: 6, marginBottom: 10, alignItems: 'center' }}>
        <SectionTab label="Cookies" count={cookies.length} active={section === 'cookies'} onClick={() => setSection('cookies')} />
        <SectionTab label="localStorage" count={Object.keys(local).length} active={section === 'local'} onClick={() => setSection('local')} />
        <SectionTab label="sessionStorage" count={Object.keys(session).length} active={section === 'session'} onClick={() => setSection('session')} />
        <div style={{ flex: 1 }} />
        <span style={{ opacity: 0.55, marginRight: 6 }}>{origin}</span>
        <button onClick={() => void refresh()} title="Refresh">↻</button>
      </div>

      {section === 'cookies' && (
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            padding: '6px 10px',
            marginBottom: 8,
            border: '1px solid #2a2a2a',
            borderRadius: 4,
            background: sticky.enabled ? 'rgba(110, 231, 183, 0.05)' : '#161616',
            fontSize: 11
          }}
        >
          <label style={{ display: 'flex', alignItems: 'center', gap: 6, cursor: 'pointer' }}>
            <input
              type="checkbox"
              checked={sticky.enabled}
              onChange={async (e) => {
                const r = await window.rev.storage.persistenceSet(e.target.checked)
                setSticky((s) => ({ ...s, enabled: r.enabled }))
              }}
            />
            <strong style={{ color: sticky.enabled ? '#6ee7b7' : '#ccc' }}>
              Sticky session cookies
            </strong>
          </label>
          <span style={{ opacity: 0.6 }}>
            Session-only cookies persist for 30 days after app exit
          </span>
          <div style={{ flex: 1 }} />
          {sticky.snapshotCount > 0 && (
            <span style={{ opacity: 0.55 }}>{sticky.snapshotCount} snapshots saved</span>
          )}
          <button
            onClick={async () => {
              const r = await window.rev.storage.persistenceSnapshot()
              setSticky((s) => ({ ...s, snapshotCount: r.snapshotCount }))
            }}
            style={{
              background: '#1a1a1a',
              border: '1px solid #333',
              color: '#ccc',
              padding: '3px 9px',
              borderRadius: 3,
              fontSize: 10,
              cursor: 'pointer'
            }}
            title="Dump session cookies to disk immediately"
          >
            Snapshot now
          </button>
        </div>
      )}

      {section === 'cookies' && (
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            padding: '6px 10px',
            marginBottom: 8,
            border: '1px solid #2a2a2a',
            borderRadius: 4,
            background: dialog.autoDismiss ? 'rgba(110, 231, 183, 0.05)' : '#161616',
            fontSize: 11
          }}
        >
          <label style={{ display: 'flex', alignItems: 'center', gap: 6, cursor: 'pointer' }}>
            <input
              type="checkbox"
              checked={dialog.autoDismiss}
              onChange={async (e) => {
                const r = await window.rev.dialog.setAutoDismiss(e.target.checked)
                setDialog((d) => ({ ...d, autoDismiss: r.autoDismiss }))
              }}
            />
            <strong style={{ color: dialog.autoDismiss ? '#6ee7b7' : '#ccc' }}>
              Auto-dismiss alert / confirm / prompt
            </strong>
          </label>
          <span style={{ opacity: 0.6 }}>
            Auto-closes dialogs so CDP/AI keep running (contents are logged)
          </span>
          <div style={{ flex: 1 }} />
          {dialog.history.length > 0 && (
            <>
              <span style={{ opacity: 0.55 }}>{dialog.history.length} recent</span>
              <button
                onClick={async () => {
                  await window.rev.dialog.clearHistory()
                  setDialog((d) => ({ ...d, history: [] }))
                }}
                style={{
                  background: '#1a1a1a',
                  border: '1px solid #333',
                  color: '#ccc',
                  padding: '3px 9px',
                  borderRadius: 3,
                  fontSize: 10,
                  cursor: 'pointer'
                }}
              >
                clear
              </button>
            </>
          )}
        </div>
      )}

      {section === 'cookies' && dialog.history.length > 0 && (
        <div
          style={{
            border: '1px solid #2a2a2a',
            borderRadius: 4,
            background: '#0e0e0e',
            marginBottom: 8,
            maxHeight: 110,
            overflow: 'auto',
            fontFamily: 'ui-monospace, monospace',
            fontSize: 10.5
          }}
        >
          {dialog.history
            .slice()
            .reverse()
            .map((d, i) => (
              <div
                key={d.ts + ':' + i}
                style={{
                  padding: '3px 8px',
                  borderBottom: '1px solid #1c1c1c',
                  display: 'flex',
                  gap: 8
                }}
              >
                <span
                  style={{
                    color:
                      d.type === 'alert'
                        ? '#fbbf24'
                        : d.type === 'confirm'
                          ? '#60a5fa'
                          : d.type === 'prompt'
                            ? '#f0abfc'
                            : '#888',
                    fontWeight: 700,
                    minWidth: 60
                  }}
                >
                  {d.type.toUpperCase()}
                </span>
                <span style={{ flex: 1, color: '#ddd', wordBreak: 'break-all' }}>{d.message}</span>
                <span style={{ opacity: 0.5 }}>{new Date(d.ts).toLocaleTimeString()}</span>
              </div>
            ))}
        </div>
      )}

      <div style={{ flex: 1, overflow: 'auto', border: '1px solid #2a2a2a', borderRadius: 4 }}>
        {section === 'cookies' && (
          <table style={tableStyle}>
            <thead style={theadStyle}>
              <tr>
                <Th>name</Th><Th>value</Th><Th>domain</Th><Th>path</Th>
                <Th>flags</Th><Th>expires</Th><Th></Th>
              </tr>
            </thead>
            <tbody>
              {cookies.map((c) => (
                <tr key={`${c.domain}|${c.path}|${c.name}`} style={trStyle}>
                  <Td><strong>{c.name}</strong></Td>
                  <Td><code style={valStyle}>{truncate(c.value, 80)}</code></Td>
                  <Td>{c.domain}</Td>
                  <Td>{c.path}</Td>
                  <Td>
                    {c.secure && <Tag>Secure</Tag>}
                    {c.httpOnly && <Tag>HttpOnly</Tag>}
                    {c.sameSite && <Tag>{c.sameSite}</Tag>}
                  </Td>
                  <Td>{c.expires ? new Date(c.expires * 1000).toISOString().slice(0, 16).replace('T', ' ') : 'session'}</Td>
                  <Td>
                    <button onClick={() => setEditCookie({ ...c })} style={btnStyle}>✎</button>
                    <button
                      style={btnStyle}
                      onClick={async () => {
                        await window.rev.storage.cookieDelete({
                          name: c.name,
                          url: origin ?? undefined,
                          domain: c.domain || undefined,
                          path: c.path || undefined
                        })
                        await refresh()
                      }}
                    >
                      ✕
                    </button>
                  </Td>
                </tr>
              ))}
              {cookies.length === 0 && (
                <tr><Td colSpan={7} style={{ opacity: 0.5, padding: 16, textAlign: 'center' }}>no cookies for {origin ?? 'this origin'}</Td></tr>
              )}
            </tbody>
          </table>
        )}

        {section !== 'cookies' && (
          <table style={tableStyle}>
            <thead style={theadStyle}>
              <tr><Th>key</Th><Th>value</Th><Th></Th></tr>
            </thead>
            <tbody>
              {Object.entries(section === 'local' ? local : session).map(([k, v]) => (
                <tr key={k} style={trStyle}>
                  <Td><strong>{k}</strong></Td>
                  <Td>
                    {editing?.key === k ? (
                      <input
                        autoFocus
                        defaultValue={editing.value}
                        onBlur={async (e) => {
                          const nv = e.currentTarget.value
                          setEditing(null)
                          if (section === 'local') await window.rev.storage.localSet(k, nv)
                          else await window.rev.storage.sessionSet(k, nv)
                          await refresh()
                        }}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter') e.currentTarget.blur()
                          if (e.key === 'Escape') setEditing(null)
                        }}
                        style={{ width: '100%', background: '#111', color: '#eee', border: '1px solid #444', padding: 2, fontFamily: 'ui-monospace, monospace', fontSize: 11 }}
                      />
                    ) : (
                      <code style={valStyle} onDoubleClick={() => setEditing({ key: k, value: v })}>{truncate(v, 120)}</code>
                    )}
                  </Td>
                  <Td>
                    <button style={btnStyle} onClick={() => setEditing({ key: k, value: v })}>✎</button>
                    <button
                      style={btnStyle}
                      onClick={async () => {
                        if (section === 'local') await window.rev.storage.localDelete(k)
                        else await window.rev.storage.sessionDelete(k)
                        await refresh()
                      }}
                    >
                      ✕
                    </button>
                  </Td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {/* Add row */}
      <div style={{ display: 'flex', gap: 6, marginTop: 8, alignItems: 'center' }}>
        {section === 'cookies' ? (
          <button
            onClick={() =>
              setNewCookie({
                name: '',
                value: '',
                domain: origin ? new URL(origin).hostname : '',
                path: '/'
              })
            }
            style={addBtn}
          >
            + cookie
          </button>
        ) : (
          <>
            {adding ? (
              <>
                <input
                  placeholder="key"
                  value={adding.key}
                  onChange={(e) => setAdding({ ...adding, key: e.target.value })}
                  style={addInput}
                />
                <input
                  placeholder="value"
                  value={adding.value}
                  onChange={(e) => setAdding({ ...adding, value: e.target.value })}
                  style={{ ...addInput, flex: 1 }}
                />
                <button
                  style={addBtn}
                  onClick={async () => {
                    if (!adding.key) return
                    if (section === 'local') await window.rev.storage.localSet(adding.key, adding.value)
                    else await window.rev.storage.sessionSet(adding.key, adding.value)
                    setAdding(null)
                    await refresh()
                  }}
                >
                  add
                </button>
                <button style={addBtn} onClick={() => setAdding(null)}>×</button>
              </>
            ) : (
              <>
                <button onClick={() => setAdding({ key: '', value: '' })} style={addBtn}>
                  + {section === 'local' ? 'localStorage' : 'sessionStorage'} entry
                </button>
                <button
                  onClick={async () => {
                    if (!confirm(`Clear all ${section}Storage?`)) return
                    if (section === 'local') await window.rev.storage.localClear()
                    else await window.rev.storage.sessionClear()
                    await refresh()
                  }}
                  style={addBtn}
                >
                  clear all
                </button>
              </>
            )}
          </>
        )}
      </div>

      {(editCookie || newCookie) && (
        <CookieEditor
          initial={(editCookie ?? newCookie)!}
          origin={origin}
          isNew={!!newCookie}
          onClose={() => {
            setEditCookie(null)
            setNewCookie(null)
          }}
          onSaved={async () => {
            setEditCookie(null)
            setNewCookie(null)
            await refresh()
          }}
        />
      )}
    </div>
  )
}

function CookieEditor({
  initial,
  origin,
  isNew,
  onClose,
  onSaved
}: {
  initial: CookieRow
  origin: string | null
  isNew: boolean
  onClose: () => void
  onSaved: () => Promise<void>
}) {
  const [c, setC] = useState<CookieRow>(initial)
  return (
    <div
      onClick={(e) => e.target === e.currentTarget && onClose()}
      style={{
        position: 'fixed',
        inset: 0,
        background: 'rgba(0,0,0,0.55)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        zIndex: 1000
      }}
    >
      <div style={{ background: '#1c1c1c', border: '1px solid #333', borderRadius: 6, padding: 18, width: 460, color: '#eee' }}>
        <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 12 }}>{isNew ? 'New cookie' : `Edit ${c.name}`}</div>
        <Field label="name">
          <input
            style={editorInput}
            value={c.name}
            onChange={(e) => setC({ ...c, name: e.target.value })}
            disabled={!isNew}
          />
        </Field>
        <Field label="value">
          <textarea
            style={{ ...editorInput, minHeight: 70, resize: 'vertical' }}
            value={c.value}
            onChange={(e) => setC({ ...c, value: e.target.value })}
          />
        </Field>
        <Field label="domain">
          <input style={editorInput} value={c.domain} onChange={(e) => setC({ ...c, domain: e.target.value })} />
        </Field>
        <Field label="path">
          <input style={editorInput} value={c.path} onChange={(e) => setC({ ...c, path: e.target.value })} />
        </Field>
        <div style={{ display: 'flex', gap: 12, fontSize: 11 }}>
          <label><input type="checkbox" checked={!!c.secure} onChange={(e) => setC({ ...c, secure: e.target.checked })} /> Secure</label>
          <label><input type="checkbox" checked={!!c.httpOnly} onChange={(e) => setC({ ...c, httpOnly: e.target.checked })} /> HttpOnly</label>
          <label>
            SameSite
            <select
              style={{ marginLeft: 4, background: '#111', color: '#eee', border: '1px solid #333' }}
              value={c.sameSite ?? ''}
              onChange={(e) => setC({ ...c, sameSite: e.target.value || undefined })}
            >
              <option value="">(none)</option>
              <option value="Strict">Strict</option>
              <option value="Lax">Lax</option>
              <option value="None">None</option>
            </select>
          </label>
        </div>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 6, marginTop: 14 }}>
          <button onClick={onClose} style={btnStyle}>Cancel</button>
          <button
            style={{ ...btnStyle, background: '#244', borderColor: '#377' }}
            onClick={async () => {
              await window.rev.storage.cookieSet({
                name: c.name,
                value: c.value,
                url: origin ?? undefined,
                domain: c.domain || undefined,
                path: c.path || undefined,
                secure: c.secure || undefined,
                httpOnly: c.httpOnly || undefined,
                sameSite: c.sameSite as 'Strict' | 'Lax' | 'None' | undefined
              })
              await onSaved()
            }}
          >
            Save
          </button>
        </div>
      </div>
    </div>
  )
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div style={{ marginBottom: 8 }}>
      <div style={{ fontSize: 10, opacity: 0.6, marginBottom: 2 }}>{label}</div>
      {children}
    </div>
  )
}

function SectionTab({ label, count, active, onClick }: { label: string; count: number; active: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      style={{
        background: active ? '#244' : '#1a1a1a',
        border: '1px solid ' + (active ? '#377' : '#333'),
        color: '#ddd',
        padding: '4px 10px',
        borderRadius: 4,
        fontSize: 11,
        cursor: 'pointer'
      }}
    >
      {label} <span style={{ opacity: 0.55, marginLeft: 4 }}>{count}</span>
    </button>
  )
}

function Tag({ children }: { children: React.ReactNode }) {
  return <span style={{ background: '#222', border: '1px solid #444', borderRadius: 3, padding: '0 4px', fontSize: 10, marginRight: 3 }}>{children}</span>
}

function truncate(s: string, n: number) {
  return s.length > n ? s.slice(0, n) + '…' : s
}

function Th({ children }: { children?: React.ReactNode }) {
  return <th style={{ textAlign: 'left', padding: '5px 7px', borderBottom: '1px solid #2a2a2a', fontWeight: 500, fontSize: 11, opacity: 0.75 }}>{children}</th>
}
function Td({ children, style, colSpan }: { children?: React.ReactNode; style?: React.CSSProperties; colSpan?: number }) {
  return <td colSpan={colSpan} style={{ padding: '4px 7px', borderBottom: '1px solid #1f1f1f', verticalAlign: 'top', ...style }}>{children}</td>
}

const tableStyle: React.CSSProperties = { width: '100%', borderCollapse: 'collapse', fontFamily: 'ui-monospace, monospace', fontSize: 11 }
const theadStyle: React.CSSProperties = { position: 'sticky', top: 0, background: '#161616' }
const trStyle: React.CSSProperties = {}
const valStyle: React.CSSProperties = { background: '#111', padding: '1px 4px', borderRadius: 2, wordBreak: 'break-all', cursor: 'text', display: 'inline-block', maxWidth: '100%' }
const btnStyle: React.CSSProperties = { background: '#1a1a1a', border: '1px solid #333', color: '#ccc', padding: '2px 6px', borderRadius: 3, fontSize: 10, cursor: 'pointer', marginLeft: 3 }
const addBtn: React.CSSProperties = { ...btnStyle, padding: '4px 10px', fontSize: 11 }
const addInput: React.CSSProperties = { background: '#111', color: '#eee', border: '1px solid #333', padding: '4px 6px', fontSize: 11, borderRadius: 3, fontFamily: 'ui-monospace, monospace' }
const editorInput: React.CSSProperties = { width: '100%', background: '#111', color: '#eee', border: '1px solid #333', padding: '4px 8px', fontFamily: 'ui-monospace, monospace', fontSize: 11, borderRadius: 3, boxSizing: 'border-box' }
