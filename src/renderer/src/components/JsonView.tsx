import { useMemo, useState } from 'react'

import { useT, type TFn } from '@/stores/i18n'

// Body viewer for the traffic drawer and the repeater. JSON gets a collapsible,
// syntax-coloured tree with a Raw toggle; anything else falls back to the plain
// <pre> the panels showed before.

// Above this the tree is more scroll than signal and the parse cost shows, so
// large bodies open in raw mode (the toggle still switches to the tree).
const TREE_MAX_CHARS = 1_000_000
// Rows rendered per container before the rest is summarised. A single API
// response can carry thousands of items; React chokes long before the reader
// scrolls that far.
const MAX_ROWS = 500
const INDENT = 14

type Json = null | boolean | number | string | Json[] | { [k: string]: Json }

const PRE_STYLE: React.CSSProperties = {
  margin: 0,
  padding: 10,
  background: 'var(--bg)',
  border: '1px solid var(--border)',
  borderRadius: 4,
  maxHeight: 360,
  overflow: 'auto',
  fontSize: 11,
  fontFamily: 'ui-monospace, monospace',
  whiteSpace: 'pre-wrap',
  wordBreak: 'break-word'
}

function parseJson(text: string, contentType?: string): Json | undefined {
  const trimmed = text.trim()
  const looksJson =
    (trimmed.startsWith('{') && trimmed.endsWith('}')) ||
    (trimmed.startsWith('[') && trimmed.endsWith(']')) ||
    !!contentType?.includes('json')
  if (!looksJson) return undefined
  try {
    return JSON.parse(trimmed) as Json
  } catch {
    return undefined
  }
}

function isContainer(v: Json): v is Json[] | { [k: string]: Json } {
  return typeof v === 'object' && v !== null
}

function Scalar({ value }: { value: Json }): React.ReactElement {
  if (value === null) return <span style={{ color: 'var(--json-null)' }}>null</span>
  if (typeof value === 'string')
    return <span style={{ color: 'var(--json-string)' }}>{JSON.stringify(value)}</span>
  if (typeof value === 'number')
    return <span style={{ color: 'var(--json-number)' }}>{String(value)}</span>
  return <span style={{ color: 'var(--json-bool)' }}>{String(value)}</span>
}

function Label({ name, index }: { name?: string; index?: number }): React.ReactElement | null {
  if (index !== undefined)
    return (
      <>
        <span style={{ color: 'var(--json-punct)' }}>{index}</span>
        <span style={{ color: 'var(--json-punct)' }}>: </span>
      </>
    )
  if (name === undefined) return null
  return (
    <>
      <span style={{ color: 'var(--json-key)' }}>{JSON.stringify(name)}</span>
      <span style={{ color: 'var(--json-punct)' }}>: </span>
    </>
  )
}

interface NodeProps {
  value: Json
  depth: number
  name?: string
  index?: number
  trailingComma: boolean
  t: TFn
}

function Node({ value, depth, name, index, trailingComma, t }: NodeProps): React.ReactElement {
  // Two levels open is enough to see the shape of a response without the tree
  // scrolling off-screen on anything nested.
  const [open, setOpen] = useState(depth < 2)
  const pad = depth * INDENT

  if (!isContainer(value)) {
    return (
      <div style={{ paddingLeft: pad + 12 }}>
        <Label name={name} index={index} />
        <Scalar value={value} />
        {trailingComma && <span style={{ color: 'var(--json-punct)' }}>,</span>}
      </div>
    )
  }

  const isArr = Array.isArray(value)
  const entries: [string, Json][] = isArr
    ? value.map((v, i) => [String(i), v])
    : Object.entries(value)
  const [openB, closeB] = isArr ? ['[', ']'] : ['{', '}']
  const count = entries.length
  const summary = isArr ? t('json.items', { n: count }) : t('json.keys', { n: count })
  const shown = entries.slice(0, MAX_ROWS)

  const caret = (
    <span
      onClick={() => setOpen((o) => !o)}
      style={{ cursor: 'pointer', color: 'var(--json-punct)', userSelect: 'none' }}
    >
      {open ? '▾ ' : '▸ '}
    </span>
  )

  if (!open) {
    return (
      <div style={{ paddingLeft: pad }}>
        {caret}
        <Label name={name} index={index} />
        <span
          onClick={() => setOpen(true)}
          style={{ cursor: 'pointer', color: 'var(--json-punct)' }}
        >
          {openB} … {summary} {closeB}
        </span>
        {trailingComma && <span style={{ color: 'var(--json-punct)' }}>,</span>}
      </div>
    )
  }

  return (
    <div>
      <div style={{ paddingLeft: pad }}>
        {caret}
        <Label name={name} index={index} />
        <span style={{ color: 'var(--json-punct)' }}>{openB}</span>
      </div>
      {shown.map(([k, v], i) => (
        <Node
          key={k}
          value={v}
          depth={depth + 1}
          {...(isArr ? { index: Number(k) } : { name: k })}
          trailingComma={i < count - 1}
          t={t}
        />
      ))}
      {count > shown.length && (
        <div style={{ paddingLeft: pad + INDENT + 12, color: 'var(--json-punct)' }}>
          {t('json.more', { n: count - shown.length })}
        </div>
      )}
      <div style={{ paddingLeft: pad + 12, color: 'var(--json-punct)' }}>
        {closeB}
        {trailingComma && ','}
      </div>
    </div>
  )
}

function ToolbarButton({
  active,
  onClick,
  children
}: {
  active?: boolean
  onClick: () => void
  children: React.ReactNode
}): React.ReactElement {
  return (
    <button
      onClick={onClick}
      style={{
        padding: '2px 8px',
        fontSize: 10,
        borderRadius: 3,
        cursor: 'pointer',
        border: `1px solid ${active ? 'var(--accent-border)' : 'var(--border)'}`,
        background: active ? 'var(--accent-soft)' : 'transparent',
        color: active ? 'var(--accent-text)' : 'var(--text-dim)'
      }}
    >
      {children}
    </button>
  )
}

export function JsonView({
  text,
  contentType
}: {
  text: string
  contentType?: string
}): React.ReactElement {
  const t = useT()
  const parsed = useMemo(() => parseJson(text, contentType), [text, contentType])
  const tooBig = text.length > TREE_MAX_CHARS
  const [pretty, setPretty] = useState(!tooBig)
  const [copied, setCopied] = useState(false)

  if (parsed === undefined) return <pre style={PRE_STYLE}>{text}</pre>

  const copy = (): void => {
    void navigator.clipboard.writeText(pretty ? JSON.stringify(parsed, null, 2) : text)
    setCopied(true)
    setTimeout(() => setCopied(false), 1200)
  }

  return (
    <div>
      <div style={{ display: 'flex', gap: 4, marginBottom: 4 }}>
        <ToolbarButton active={pretty} onClick={() => setPretty(true)}>
          {t('json.pretty')}
        </ToolbarButton>
        <ToolbarButton active={!pretty} onClick={() => setPretty(false)}>
          {t('json.raw')}
        </ToolbarButton>
        <ToolbarButton onClick={copy}>{copied ? t('json.copied') : t('json.copy')}</ToolbarButton>
      </div>
      {pretty ? (
        <div style={{ ...PRE_STYLE, whiteSpace: 'pre', lineHeight: 1.5 }}>
          <Node value={parsed} depth={0} trailingComma={false} t={t} />
        </div>
      ) : (
        <pre style={PRE_STYLE}>{text}</pre>
      )}
    </div>
  )
}
