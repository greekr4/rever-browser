import { useEffect, useMemo, useRef, useState } from 'react'

import type { WorkflowEditorProps } from '../core/registry'
import {
  EXTRACT_NOTE_CAPPED,
  EXTRACT_NOTE_NONE,
  attachExtract,
  capturedFlagValues,
  cliRunExample,
  commandFlagNames,
  exportCurl,
  runJsonBody,
  type CliData
} from '../../../../shared/cli-spec'

import { JsonView } from '@/components/JsonView'
import { useT, type TFn } from '@/stores/i18n'
import { useTrafficStore } from '@/stores/traffic'

const API_TYPES = new Set(['XHR', 'Fetch'])

function asCli(data: unknown): CliData {
  const row = (data ?? {}) as Partial<CliData>
  return {
    site: row.site ?? '',
    title: row.title ?? '',
    baseUrl: row.baseUrl ?? '',
    commands: Array.isArray(row.commands) ? row.commands.map((command) => attachExtract(command)) : [],
    writes: row.writes === true
  }
}

interface CliRunView {
  status?: number
  body?: string
  bodyCut?: boolean
  items?: Array<Record<string, string>>
  matched?: number
  note?: string
}

function noteText(note: string | undefined, tr: TFn): string | undefined {
  if (!note) return undefined
  const missed = note.includes(EXTRACT_NOTE_NONE)
  const capped = note.includes(EXTRACT_NOTE_CAPPED)
  if (missed && capped) return `${tr('workflow.cli.noMatchExtract')} ${tr('workflow.cli.pageCapped')}`
  if (missed) return tr('workflow.cli.noMatchExtract')
  if (capped) return tr('workflow.cli.pageCapped')
  return note
}

function formatRun(result: CliRunView, tr: TFn): string {
  if (result.items) {
    const rows = result.items.map((item, index) => {
      const cells = Object.entries(item)
        .map(([key, value]) => `${key}: ${value}`)
        .join('  ·  ')
      return `${index + 1}. ${cells}`
    })
    const tail = [tr('workflow.cli.matched', { count: result.matched ?? result.items.length })]
    const note = noteText(result.note, tr)
    if (note) tail.push(note)
    return [`${result.status ?? ''}`, ...rows, ...tail].filter((line) => line !== '').join('\n')
  }
  const lines = [`${result.status ?? ''}`, result.body ?? '']
  if (result.bodyCut) lines.push(tr('workflow.cli.bodyCut'))
  return lines.filter((line) => line !== '').join('\n')
}



export function CliEditor({ workflow, onChange }: WorkflowEditorProps): React.ReactElement {
  const tr = useT()
  const entries = useTrafficStore((s) => s.entries)
  const order = useTrafficStore((s) => s.order)
  const data = asCli(workflow.data)
  const [picked, setPicked] = useState<Set<string>>(new Set())
  const [busy, setBusy] = useState<'compile' | 'run' | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [exported, setExported] = useState<string | null>(null)
  const [active, setActive] = useState(data.commands[0]?.name ?? '')
  const [inputs, setInputs] = useState<Record<string, string>>({})
  const [runText, setRunText] = useState<string | null>(null)
  // A JSON run body renders in the collapsible viewer instead of one raw line.
  const [runJson, setRunJson] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)
  const seeded = useRef(new Set<string>())
  // Bring a fresh run result or export into view: both render below the
  // flags, often past the bottom of a short panel.
  const exportRef = useRef<HTMLDivElement>(null)
  const runRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (exported != null) exportRef.current?.scrollIntoView({ block: 'start', behavior: 'smooth' })
  }, [exported])
  useEffect(() => {
    if (runText != null) runRef.current?.scrollIntoView({ block: 'start', behavior: 'smooth' })
  }, [runText, runJson])

  const calls = useMemo(() => {
    const rows = []
    for (let i = order.length - 1; i >= 0 && rows.length < 40; i--) {
      const entry = entries[order[i]]
      if (!entry || !API_TYPES.has(entry.resourceType)) continue
      rows.push(entry)
    }
    return rows
  }, [entries, order])

  const command = data.commands.find((item) => item.name === active) ?? data.commands[0]
  const flags = command ? commandFlagNames(command) : []
  const example = command ? cliRunExample(workflow.id, command, inputs) : ''

  // Fill empty flags once from the captured URL while that request is still in
  // the traffic list. The value is not written back onto the saved workflow.
  useEffect(() => {
    if (!command) return
    const sources = command.evidenceRequestIds
      .map((id) => entries[id])
      .filter((entry) => entry != null)
      .map((entry) => ({ url: entry.url }))
    const found = capturedFlagValues(command, sources)
    const markPrefix = `${workflow.id}:${command.name}:`
    setInputs((prev) => {
      let changed = false
      const next = { ...prev }
      for (const [name, value] of Object.entries(found)) {
        const mark = `${markPrefix}${name}`
        if (seeded.current.has(mark)) continue
        seeded.current.add(mark)
        if (!next[name]) {
          next[name] = value
          changed = true
        }
      }
      return changed ? next : prev
    })
  }, [command, entries, workflow.id])

  const patch = (next: Partial<CliData>, name?: string): void => {
    onChange({
      ...workflow,
      ...(name != null ? { name } : {}),
      data: { ...data, ...next }
    })
  }

  const compile = async (): Promise<void> => {
    setBusy('compile')
    setError(null)
    setExported(null)
    try {
      const compiled = await window.rev.workflows.compileCli([...picked])
      if (compiled.commands.length === 0) {
        setError(tr('workflow.cli.noMatch'))
        return
      }
      const name = workflow.name.trim() ? workflow.name : compiled.commands[0]?.name || compiled.site
      patch({ ...compiled, writes: false }, name)
      setActive(compiled.commands[0]?.name ?? '')
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(null)
    }
  }

  const run = async (): Promise<void> => {
    if (!command) return
    setBusy('run')
    setError(null)
    setRunText(null)
    setRunJson(null)
    try {
      const result = await window.rev.workflows.runCli({
        baseUrl: data.baseUrl,
        writes: data.writes,
        command,
        inputs
      })
      if (!result.ok) {
        setRunText(result.error ?? tr('workflow.cli.runFailed'))
        return
      }
      const json = runJsonBody(result)
      setRunJson(json)
      setRunText(json != null ? `${result.status ?? ''}` : formatRun(result, tr))
    } catch (e) {
      setRunText(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(null)
    }
  }

  return (
    <div data-testid="cli-editor" style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
      <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)' }}>
        {tr('workflow.cli.name')}
        <input
          value={workflow.name}
          onChange={(e) => onChange({ ...workflow, name: e.target.value })}
          style={{ height: 28, padding: '0 8px' }}
        />
      </label>

      <div style={{ color: 'var(--text-dim)' }}>{tr('workflow.cli.requests')}</div>
      {calls.length === 0 ? (
        <div style={{ color: 'var(--text-dim)' }}>{tr('workflow.cli.empty')}</div>
      ) : (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 4, maxHeight: 180, overflowY: 'auto' }}>
          {calls.map((entry) => {
            let path = entry.url
            try {
              const url = new URL(entry.url)
              path = `${url.pathname}${url.search}`
            } catch {
              // Keep the raw url when it is not absolute.
            }
            return (
              <label key={entry.requestId} data-testid={`cli-request-${entry.requestId}`} style={{ display: 'flex', gap: 6 }}>
                <input
                  type="checkbox"
                  checked={picked.has(entry.requestId)}
                  onChange={() => {
                    setPicked((prev) => {
                      const next = new Set(prev)
                      if (next.has(entry.requestId)) next.delete(entry.requestId)
                      else next.add(entry.requestId)
                      return next
                    })
                  }}
                />
                <span>
                  {entry.method} {path}
                </span>
              </label>
            )
          })}
        </div>
      )}
      <div>
        <button type="button" data-testid="cli-compile" onClick={() => void compile()} disabled={busy != null || picked.size === 0}>
          {busy === 'compile' ? tr('workflow.cli.compiling') : tr('workflow.cli.compile')}
        </button>
      </div>

      <div style={{ color: 'var(--text-dim)' }}>{tr('workflow.cli.commands')}</div>
      {data.commands.length === 0 ? (
        <div style={{ color: 'var(--text-dim)' }}>{tr('workflow.cli.noCommands')}</div>
      ) : (
        data.commands.map((item) => (
          <button
            key={item.name}
            type="button"
            data-testid="cli-command"
            data-name={item.name}
            onClick={() => setActive(item.name)}
            style={{
              textAlign: 'left',
              borderColor: item.name === command?.name ? 'var(--accent)' : undefined
            }}
          >
            {item.name} · {item.method} {item.path} · {item.effect}
          </button>
        ))
      )}

      <label style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
        <input
          data-testid="cli-writes"
          type="checkbox"
          checked={data.writes}
          onChange={(e) => patch({ writes: e.target.checked })}
        />
        {tr('workflow.cli.writes')}
      </label>
      <div style={{ color: 'var(--text-dim)' }}>{tr('workflow.cli.writesHint')}</div>
      <div style={{ color: 'var(--text-dim)' }}>{tr('workflow.cli.sessionHint')}</div>

      <div style={{ display: 'flex', gap: 6 }}>
        <button
          type="button"
          data-testid="cli-export"
          disabled={data.commands.length === 0}
          onClick={() => setExported(exportCurl(data))}
        >
          {tr('workflow.cli.export')}
        </button>
        <button type="button" data-testid="cli-run" disabled={!command || busy != null} onClick={() => void run()}>
          {busy === 'run' ? tr('workflow.cli.running') : tr('workflow.cli.run')}
        </button>
      </div>

      {command && flags.length > 0 && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
          {flags.map((name) => (
            <label key={name} style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)' }}>
              {name}
              <input
                data-testid={`cli-flag-${name}`}
                value={inputs[name] ?? ''}
                onChange={(e) => setInputs((prev) => ({ ...prev, [name]: e.target.value }))}
                style={{ height: 28, padding: '0 8px' }}
              />
            </label>
          ))}
        </div>
      )}

      {command && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8 }}>
            <span style={{ color: 'var(--text-dim)' }}>{tr('workflow.cli.callExample')}</span>
            <button
              type="button"
              data-testid="cli-example-copy"
              onClick={() => {
                void navigator.clipboard.writeText(example).then(() => {
                  setCopied(true)
                  window.setTimeout(() => setCopied(false), 1200)
                })
              }}
            >
              {copied ? tr('cli.copied') : tr('cli.copy')}
            </button>
          </div>
          <pre data-testid="cli-call-example" style={{ whiteSpace: 'pre-wrap', margin: 0 }}>
            {example}
          </pre>
          <div style={{ color: 'var(--text-dim)' }}>{tr('workflow.cli.callExampleHint')}</div>
        </div>
      )}

      {command?.extract && (
        <div style={{ color: 'var(--text-dim)', overflowWrap: 'anywhere' }}>
          {tr('workflow.cli.extract', {
            limit: String(command.extract.limit),
            selector: command.extract.selector
          })}
        </div>
      )}

      {exported != null && (
        <div ref={exportRef} style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
          <div style={{ color: 'var(--text-dim)' }}>{tr('workflow.cli.exportHeading')}</div>
          <pre data-testid="cli-export-text" style={{ whiteSpace: 'pre-wrap', margin: 0 }}>
            {exported}
          </pre>
        </div>
      )}
      {runText != null && (
        <div ref={runRef} style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
          <div style={{ color: 'var(--text-dim)' }}>{tr('workflow.cli.resultHeading')}</div>
          <div data-testid="cli-run-result">
            <pre style={{ whiteSpace: 'pre-wrap', margin: 0 }}>{runText}</pre>
            {runJson != null && <JsonView text={runJson} contentType="application/json" />}
          </div>
        </div>
      )}
      {error && <div style={{ color: '#e06c6c' }}>{error}</div>}
    </div>
  )
}
