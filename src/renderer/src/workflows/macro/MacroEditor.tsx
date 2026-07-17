import { useEffect, useMemo, useRef, useState } from 'react'

import type { WorkflowEditorProps } from '../core/registry'

// Structural mirrors of the preload types (window.rev.workflows.*). Kept local
// so the macro module stays self-contained and disposable.
interface McpToolInfo {
  name: string
  description?: string
  inputSchema: unknown
}
interface WorkflowRunStep {
  tool: string
  input: Record<string, unknown>
}
interface WorkflowStepProgress {
  index: number
  tool: string
  status: 'running' | 'done' | 'error'
  output?: string
  error?: string
}

export interface MacroStep {
  id: string
  tool: string
  // JSON object literal (as text) for the tool arguments. May contain {{var}}.
  input: string
}

export interface MacroData {
  steps: MacroStep[]
  // JSON object (as text) supplying values for {{var}} placeholders.
  vars: string
}

function stepId(): string {
  return `s${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`
}

// Substitute {{var}} then parse each step's input into a runnable form.
function resolveSteps(
  steps: MacroStep[],
  varsText: string
): { ok: true; steps: WorkflowRunStep[] } | { ok: false; error: string } {
  let vars: Record<string, unknown> = {}
  if (varsText.trim()) {
    try {
      vars = JSON.parse(varsText)
    } catch {
      return { ok: false, error: 'Variables are not valid JSON' }
    }
  }
  const out: WorkflowRunStep[] = []
  for (const s of steps) {
    if (!s.tool) return { ok: false, error: 'A step has no tool selected' }
    const substituted = s.input.replace(/\{\{\s*([\w.-]+)\s*\}\}/g, (_m, key: string) => {
      const v = vars[key]
      return v == null ? '' : String(v)
    })
    let input: Record<string, unknown> = {}
    if (substituted.trim()) {
      try {
        input = JSON.parse(substituted)
      } catch {
        return { ok: false, error: `Step "${s.tool}" input is not valid JSON` }
      }
    }
    out.push({ tool: s.tool, input })
  }
  return { ok: true, steps: out }
}

export function MacroEditor({ workflow, onChange }: WorkflowEditorProps): React.ReactElement {
  const data = workflow.data as MacroData
  const [tools, setTools] = useState<McpToolInfo[]>([])
  const [running, setRunning] = useState(false)
  const [progress, setProgress] = useState<Record<number, WorkflowStepProgress>>({})
  const [runError, setRunError] = useState<string | null>(null)
  const toolsByName = useMemo(() => new Map(tools.map((t) => [t.name, t])), [tools])
  const mounted = useRef(true)

  useEffect(() => {
    mounted.current = true
    void window.rev.workflows.listTools().then((t) => {
      if (mounted.current) setTools(t)
    })
    return () => {
      mounted.current = false
    }
  }, [])

  const setData = (patch: Partial<MacroData>): void =>
    onChange({ ...workflow, data: { ...data, ...patch } })

  const setSteps = (steps: MacroStep[]): void => setData({ steps })

  const addStep = (): void =>
    setSteps([...data.steps, { id: stepId(), tool: tools[0]?.name ?? '', input: '{}' }])

  const updateStep = (id: string, patch: Partial<MacroStep>): void =>
    setSteps(data.steps.map((s) => (s.id === id ? { ...s, ...patch } : s)))

  const removeStep = (id: string): void => setSteps(data.steps.filter((s) => s.id !== id))

  const moveStep = (index: number, dir: -1 | 1): void => {
    const next = [...data.steps]
    const j = index + dir
    if (j < 0 || j >= next.length) return
    ;[next[index], next[j]] = [next[j], next[index]]
    setSteps(next)
  }

  const run = async (): Promise<void> => {
    const resolved = resolveSteps(data.steps, data.vars)
    if (!resolved.ok) {
      setRunError(resolved.error)
      return
    }
    if (resolved.steps.length === 0) {
      setRunError('No steps to run')
      return
    }
    setRunError(null)
    setProgress({})
    setRunning(true)
    try {
      await window.rev.workflows.run(resolved.steps, (p) => {
        setProgress((prev) => ({ ...prev, [p.index]: p }))
      })
    } catch (e) {
      setRunError(e instanceof Error ? e.message : String(e))
    } finally {
      if (mounted.current) setRunning(false)
    }
  }

  const cancel = (): void => {
    void window.rev.workflows.cancel()
  }

  const statusColor = (s: WorkflowStepProgress | undefined): string => {
    if (!s) return 'var(--text-dim)'
    if (s.status === 'running') return 'var(--accent)'
    if (s.status === 'error') return '#e06c6c'
    return 'var(--chip-ok-text)'
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
      <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)' }}>
        Name
        <input
          value={workflow.name}
          onChange={(e) => onChange({ ...workflow, name: e.target.value })}
          placeholder="Login then dump the profile API"
          style={{ height: 28, padding: '0 8px' }}
        />
      </label>

      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <span style={{ color: 'var(--text-dim)' }}>Steps</span>
          <button type="button" onClick={addStep} style={{ marginLeft: 'auto' }} disabled={tools.length === 0}>
            + Add step
          </button>
        </div>

        {data.steps.length === 0 && (
          <div style={{ color: 'var(--text-dim)', fontStyle: 'italic' }}>
            {tools.length === 0 ? 'Loading tools…' : 'No steps yet.'}
          </div>
        )}

        {data.steps.map((s, i) => {
          const prog = progress[i]
          const info = toolsByName.get(s.tool)
          return (
            <div
              key={s.id}
              style={{
                border: '1px solid var(--border)',
                borderRadius: 6,
                padding: 8,
                display: 'flex',
                flexDirection: 'column',
                gap: 6,
                background: 'var(--surface)'
              }}
            >
              <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                <span style={{ color: 'var(--text-dim)', width: 18 }}>{i + 1}</span>
                <select
                  value={s.tool}
                  onChange={(e) => updateStep(s.id, { tool: e.target.value })}
                  style={{ flex: 1, height: 26, padding: '0 6px' }}
                >
                  {!s.tool && <option value="">Select a tool…</option>}
                  {tools.map((t) => (
                    <option key={t.name} value={t.name}>
                      {t.name}
                    </option>
                  ))}
                </select>
                <button type="button" onClick={() => moveStep(i, -1)} disabled={i === 0} title="Move up">↑</button>
                <button type="button" onClick={() => moveStep(i, 1)} disabled={i === data.steps.length - 1} title="Move down">↓</button>
                <button type="button" onClick={() => removeStep(s.id)} title="Remove step">✕</button>
              </div>

              {info?.description && (
                <div style={{ color: 'var(--text-dim)', fontSize: 11, lineHeight: 1.35 }}>
                  {info.description}
                </div>
              )}

              <textarea
                value={s.input}
                onChange={(e) => updateStep(s.id, { input: e.target.value })}
                spellCheck={false}
                rows={3}
                placeholder='{ "url": "https://..." }'
                style={{
                  fontFamily: 'ui-monospace, monospace',
                  fontSize: 11,
                  padding: 6,
                  resize: 'vertical',
                  background: 'var(--surface-2)',
                  color: 'var(--text)',
                  border: '1px solid var(--border-2)',
                  borderRadius: 4
                }}
              />

              {info != null && (
                <details>
                  <summary style={{ cursor: 'pointer', color: 'var(--text-dim)', fontSize: 11 }}>
                    input schema
                  </summary>
                  <pre
                    style={{
                      margin: '4px 0 0',
                      padding: 6,
                      maxHeight: 140,
                      overflow: 'auto',
                      background: 'var(--code-bg)',
                      borderRadius: 4,
                      fontSize: 10.5
                    }}
                  >
                    {JSON.stringify(info.inputSchema, null, 2)}
                  </pre>
                </details>
              )}

              {prog && (
                <div style={{ fontSize: 11, color: statusColor(prog) }}>
                  {prog.status === 'running' && '● running…'}
                  {prog.status === 'done' && `✓ ${(prog.output ?? '').slice(0, 200) || 'done'}`}
                  {prog.status === 'error' && `✕ ${prog.error ?? 'failed'}`}
                </div>
              )}
            </div>
          )
        })}
      </div>

      <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)' }}>
        Variables (JSON, optional) — reference as {'{{name}}'} in step inputs
        <textarea
          value={data.vars}
          onChange={(e) => setData({ vars: e.target.value })}
          spellCheck={false}
          rows={2}
          placeholder='{ "user": "admin" }'
          style={{
            fontFamily: 'ui-monospace, monospace',
            fontSize: 11,
            padding: 6,
            resize: 'vertical',
            background: 'var(--surface-2)',
            color: 'var(--text)',
            border: '1px solid var(--border-2)',
            borderRadius: 4
          }}
        />
      </label>

      {runError && <div style={{ color: '#e06c6c', fontSize: 12 }}>{runError}</div>}

      <div style={{ display: 'flex', gap: 6 }}>
        {running ? (
          <button type="button" onClick={cancel} style={{ background: 'var(--surface-3)' }}>
            Stop
          </button>
        ) : (
          <button
            type="button"
            onClick={run}
            disabled={data.steps.length === 0}
            style={{ background: 'var(--accent)', color: '#fff', borderColor: 'var(--accent)' }}
          >
            ▶ Run macro
          </button>
        )}
        <span style={{ color: 'var(--text-dim)', fontSize: 11, alignSelf: 'center' }}>
          Runs on the active tab. Stops at the first failing step.
        </span>
      </div>
    </div>
  )
}
